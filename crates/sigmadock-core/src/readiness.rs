//! Advisory merge readiness from observed facts; never authorizes a merge.
use crate::{Checks, CiPreview, PullRequestState, Review, SessionState, Worker};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitReadiness {
    pub head: String,
    pub base: Option<String>,
    pub dirty: Vec<String>,
    pub dirty_truncated: bool,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub unpushed: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
    pub resolved: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPreview {
    pub head_sha: String,
    pub comments: Vec<ReviewComment>,
    pub complete: bool,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub worker: Worker,
    pub git: Option<GitReadiness>,
    pub git_error: Option<String>,
    pub ci: Option<CiPreview>,
    pub ci_error: Option<String>,
    pub review: Option<ReviewPreview>,
    pub review_error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub blockers: Vec<(&'static str, String)>,
    pub unknown: Vec<String>,
}
impl Readiness {
    pub fn label(&self) -> String {
        if !self.blockers.is_empty() {
            format!("Blocked by {}", self.blockers.len())
        } else if !self.unknown.is_empty() {
            "Unknown".into()
        } else {
            "Ready".into()
        }
    }
}
impl ReadinessReport {
    pub fn readiness(&self) -> Readiness {
        let mut result = Readiness {
            blockers: Vec::new(),
            unknown: Vec::new(),
        };
        let facts = &self.worker.facts;
        if let Some(warning) = &self.worker.base_warning {
            result.unknown.push(warning.clone());
        }
        if let Some(git) = &self.git {
            if !git.dirty.is_empty() {
                result.blockers.push(("Git", "Uncommitted changes".into()));
            }
            if git.unpushed.is_some_and(|n| n > 0) {
                result.blockers.push(("Git", "Unpushed commits".into()));
            }
            if git.behind.is_some_and(|n| n > 0) {
                result
                    .blockers
                    .push(("Git", "Behind the cached base branch".into()));
            }
            if git.base.is_none()
                || git.behind.is_none()
                || git.unpushed.is_none()
                || git.dirty_truncated
            {
                result.unknown.push("Git comparison is incomplete".into());
            }
            if facts
                .head_sha
                .as_ref()
                .is_some_and(|head| head != &git.head)
            {
                result
                    .unknown
                    .push("Local HEAD differs from the observed PR commit".into());
            }
        } else {
            result.unknown.push("Git state unavailable".into());
        }
        if let Some(error) = &self.git_error {
            result.unknown.push(format!("Git: {error}"));
        }
        if self.worker.forge.is_none() || facts.forge_error.is_some() {
            result.unknown.push(
                facts
                    .forge_error
                    .clone()
                    .unwrap_or_else(|| "Forge is not configured".into()),
            );
        }
        match facts.pr {
            PullRequestState::None => result
                .blockers
                .push(("Pull request", "No observed pull request".into())),
            PullRequestState::Draft => result
                .blockers
                .push(("Pull request", "Pull request is a draft".into())),
            PullRequestState::Closed => result
                .blockers
                .push(("Pull request", "Pull request is closed".into())),
            PullRequestState::Merged => result
                .unknown
                .push("Pull request has already merged".into()),
            PullRequestState::Open => {}
        }
        let mut failures = 0;
        if let Some(ci) = &self.ci {
            if ci.head_sha != ci.current_head || facts.head_sha.as_deref() != Some(&ci.head_sha) {
                result
                    .unknown
                    .push("CI results do not match the observed PR commit".into());
            }
            if !ci.complete || ci.truncated || ci.entries.is_empty() {
                result.unknown.push("CI results are incomplete".into());
            }
            for entry in &ci.entries {
                match entry.state.as_str() {
                    "failed" | "cancelled" => {
                        failures += 1;
                        result
                            .blockers
                            .push(("CI", format!("{}: {}", entry.name, entry.state)));
                    }
                    "passed" => {}
                    state => result.unknown.push(format!("CI {}: {state}", entry.name)),
                }
            }
        } else {
            result.unknown.push("CI details unavailable".into());
        }
        match facts.checks {
            Checks::Failed if failures == 0 => {
                result.blockers.push(("CI", "Observed CI failure".into()))
            }
            Checks::Unknown | Checks::Pending => result
                .unknown
                .push(format!("Observed checks: {:?}", facts.checks)),
            _ => {}
        }
        if let Some(error) = &self.ci_error {
            result.unknown.push(format!("CI: {error}"));
        }
        match facts.review {
            Review::ChangesRequested => {
                result.blockers.push(("Review", "Changes requested".into()))
            }
            Review::Pending => result.blockers.push(("Review", "Awaiting approval".into())),
            Review::Unknown => result.unknown.push("Review decision unknown".into()),
            Review::Approved => {}
        }
        if let Some(review) = &self.review {
            if !review.complete || facts.head_sha.as_deref() != Some(&review.head_sha) {
                result
                    .unknown
                    .push("Review snapshot is incomplete or belongs to another commit".into());
            }
            let mut files = std::collections::BTreeSet::new();
            for comment in &review.comments {
                match comment.resolved {
                    Some(false) => {
                        files.insert(comment.path.clone());
                    }
                    None => result
                        .unknown
                        .push(format!("Comment resolution unknown: {}", comment.path)),
                    Some(true) => {}
                }
            }
            for file in files {
                result
                    .blockers
                    .push(("Review", format!("Unresolved comments in {file}")));
            }
        } else {
            result.unknown.push("Review comments unavailable".into());
        }
        if let Some(error) = &self.review_error {
            result.unknown.push(format!("Review: {error}"));
        }
        match facts.mergeable {
            Some(false) => result
                .blockers
                .push(("Conflicts", "Merge conflict reported".into())),
            None => result.unknown.push("Mergeability unknown".into()),
            Some(true) => {}
        }
        if crate::status(facts) == crate::Status::NeedsYou
            && (matches!(facts.session, SessionState::NeedsInput | SessionState::Lost)
                || facts.exit_code.is_some_and(|code| code != 0))
        {
            result
                .blockers
                .push(("Worker", "Worker needs attention".into()));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> ReadinessReport {
        let mut worker: Worker = serde_json::from_value(serde_json::json!({
            "id":"w","project_id":"p","title":"task","agent":"claude","branch":"sigma/w","worktree":"/w","port":4200,"created_at":0,"archived":false,
            "facts":crate::Facts::default(),"forge":{"kind":"github","api_url":"https://api.github.com","owner":"owner","repo":"repo","token_env":"TOKEN"}
        })).unwrap();
        worker.facts.pr = PullRequestState::Open;
        worker.facts.checks = Checks::Passed;
        worker.facts.review = Review::Approved;
        worker.facts.mergeable = Some(true);
        worker.facts.head_sha = Some("head".into());
        ReadinessReport {
            worker,
            git: Some(GitReadiness {
                head: "head".into(),
                base: Some("origin/main".into()),
                dirty: vec![],
                dirty_truncated: false,
                ahead: Some(1),
                behind: Some(0),
                unpushed: Some(0),
            }),
            git_error: None,
            ci: Some(CiPreview {
                complete: true,
                head_sha: "head".into(),
                current_head: "head".into(),
                refreshed_at: 1,
                entries: vec![crate::CiEntry {
                    id: "ci".into(),
                    kind: "check".into(),
                    name: "tests".into(),
                    state: "passed".into(),
                    url: None,
                    details: String::new(),
                    truncated: false,
                }],
                warnings: vec![],
                truncated: false,
            }),
            ci_error: None,
            review: Some(ReviewPreview {
                head_sha: "head".into(),
                comments: vec![],
                complete: true,
                text: String::new(),
            }),
            review_error: None,
        }
    }
    #[test]
    fn readiness_requires_complete_matching_facts() {
        assert_eq!(ready().readiness().label(), "Ready");
        let mut report = ready();
        report.ci_error = Some("unauthorized".into());
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.worker.facts.forge_error = Some("timeout".into());
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.git.as_mut().unwrap().head = "new".into();
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.ci.as_mut().unwrap().current_head = "new".into();
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.review.as_mut().unwrap().complete = false;
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.ci.as_mut().unwrap().complete = false;
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.ci.as_mut().unwrap().entries.clear();
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.worker.facts.mergeable = None;
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.worker.facts.checks = Checks::Unknown;
        assert_eq!(report.readiness().label(), "Unknown");
        let mut report = ready();
        report.worker.facts.pr = PullRequestState::Merged;
        assert_ne!(report.readiness().label(), "Ready");
    }
    #[test]
    fn blockers_override_approvals_and_unknowns_in_section_order() {
        let mut report = ready();
        report
            .git
            .as_mut()
            .unwrap()
            .dirty
            .push(" M src/lib.rs".into());
        report.git.as_mut().unwrap().behind = Some(2);
        report.worker.facts.pr = PullRequestState::Draft;
        report.ci.as_mut().unwrap().entries[0].state = "failed".into();
        report
            .review
            .as_mut()
            .unwrap()
            .comments
            .push(ReviewComment {
                id: "r".into(),
                path: "src/lib.rs".into(),
                line: Some(1),
                body: "Fix this".into(),
                resolved: Some(false),
            });
        report.worker.facts.mergeable = Some(false);
        report.ci_error = Some("timeout".into());
        let result = report.readiness();
        assert_eq!(result.label(), "Blocked by 6");
        assert_eq!(
            result
                .blockers
                .iter()
                .map(|(section, _)| *section)
                .collect::<Vec<_>>(),
            vec!["Git", "Git", "Pull request", "CI", "Review", "Conflicts"]
        );
        let mut report = ready();
        report.worker.facts.session = SessionState::NeedsInput;
        assert_eq!(report.readiness().label(), "Blocked by 1");
    }
    #[test]
    fn unknown_resolution_never_counts_as_passing() {
        let mut report = ready();
        report
            .review
            .as_mut()
            .unwrap()
            .comments
            .push(ReviewComment {
                id: "r".into(),
                path: "lib.rs".into(),
                line: None,
                body: "Check this".into(),
                resolved: None,
            });
        assert_eq!(report.readiness().label(), "Unknown");
    }
}
