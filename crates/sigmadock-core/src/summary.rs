//! Deterministic Markdown session summaries built from recorded facts and git history.
//! Nothing here infers intent: every line restates a fact, a commit subject or a diff stat.
use crate::{Checks, PullRequestState, Review, SessionState, Worker, status};
use serde::{Deserialize, Serialize};

/// Most commit subjects listed; the rest are counted.
pub const MAX_COMMITS: usize = 40;
const MAX_GROUPS: usize = 12;
const MAX_PROMPT: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// `None` for binary files.
    pub added: Option<u64>,
    pub removed: Option<u64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Changes {
    /// The latest commit subjects since the fork point, oldest first.
    pub commits: Vec<String>,
    /// Older commits that were not listed.
    pub more_commits: usize,
    pub files: Vec<FileChange>,
    /// The worktree still holds uncommitted or untracked files.
    pub uncommitted: bool,
    /// Why git history is incomplete or unavailable, when it is.
    pub note: Option<String>,
}

pub fn markdown(worker: &Worker, project: &str, changes: &Changes, now: u64) -> String {
    let facts = &worker.facts;
    let title = one_line(&worker.title);
    let mut out = String::from("---\n");
    let mut field = |key: &str, value: &str| out.push_str(&format!("{key}: {value}\n"));
    field("title", &quoted(&title));
    field("project", &quoted(project));
    field("agent", &quoted(&worker.agent));
    field("branch", &quoted(&worker.branch));
    field("status", &slug(status(facts).label()));
    field("session", &session(worker));
    if let Some(url) = &facts.pr_url {
        field("pr", &quoted(url));
    }
    if facts.checks != Checks::Unknown {
        field("checks", &slug(&format!("{:?}", facts.checks)));
    }
    if facts.review != Review::Unknown {
        field("review", &slug(&format!("{:?}", facts.review)));
    }
    field("started", &timestamp(worker.created_at));
    let ended = worker.finished_at.or(worker.archived_at);
    if let Some(at) = ended {
        field("finished", &timestamp(at));
    }
    let running = matches!(
        facts.session,
        SessionState::Running | SessionState::Idle | SessionState::NeedsInput
    ) && !worker.archived;
    if let Some(end) = ended.or(running.then_some(now)) {
        field("duration", &duration(end.saturating_sub(worker.created_at)));
    }
    field("archived", if worker.archived { "true" } else { "false" });
    field("tags", "[sigmadock]");
    out.push_str("---\n\n");

    out.push_str(&format!("# {title}\n\n**Outcome:** {}\n", outcome(worker)));
    if let Some(prompt) = worker
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        out.push_str("\n## Task\n\n");
        let mut text: String = prompt.chars().take(MAX_PROMPT).collect();
        if text.len() < prompt.len() {
            text.push('…');
        }
        for line in text.lines() {
            if line.trim().is_empty() {
                out.push_str(">\n");
            } else {
                out.push_str(&format!("> {line}\n"));
            }
        }
    }

    out.push_str("\n## What changed\n\n");
    if changes.commits.is_empty() {
        out.push_str("_No commits on this branch._\n");
    }
    if changes.more_commits > 0 {
        out.push_str(&format!(
            "- _{} earlier {} not listed_\n",
            changes.more_commits,
            plural(changes.more_commits, "commit", "commits")
        ));
    }
    for subject in &changes.commits {
        out.push_str(&format!("- {}\n", one_line(subject)));
    }
    if changes.uncommitted {
        out.push_str("\n_Uncommitted changes remain in the worktree._\n");
    }
    if let Some(note) = &changes.note {
        out.push_str(&format!("\n_{}_\n", one_line(note)));
    }

    if !changes.files.is_empty() {
        let (added, removed) = totals(&changes.files);
        out.push_str(&format!(
            "\n## Files\n\n{} {} changed, +{added} −{removed}\n\n",
            changes.files.len(),
            plural(changes.files.len(), "file", "files"),
        ));
        let groups = group(&changes.files);
        for (name, count, added, removed) in groups.iter().take(MAX_GROUPS) {
            out.push_str(&format!(
                "- `{name}` — {count} {}, +{added} −{removed}\n",
                plural(*count, "file", "files")
            ));
        }
        if groups.len() > MAX_GROUPS {
            out.push_str(&format!(
                "- …and {} more locations\n",
                groups.len() - MAX_GROUPS
            ));
        }
    }
    out
}

fn outcome(worker: &Worker) -> String {
    let facts = &worker.facts;
    let mut parts = vec![status(facts).label().to_owned()];
    let pr = match facts.pr {
        PullRequestState::None => None,
        PullRequestState::Draft => Some("draft"),
        PullRequestState::Open => Some("open"),
        PullRequestState::Merged => Some("merged"),
        PullRequestState::Closed => Some("closed"),
    };
    match (pr, pr_number(facts.pr_url.as_deref())) {
        (Some(state), Some(number)) => parts.push(format!("PR #{number} {state}")),
        (Some(state), None) => parts.push(format!("PR {state}")),
        (None, _) => parts.push("no PR".into()),
    }
    match facts.checks {
        Checks::Passed => parts.push("checks passed".into()),
        Checks::Failed => parts.push("checks failed".into()),
        Checks::Pending => parts.push("checks pending".into()),
        Checks::Unknown => {}
    }
    match facts.review {
        Review::Approved => parts.push("approved".into()),
        Review::ChangesRequested => parts.push("changes requested".into()),
        Review::Pending => parts.push("review pending".into()),
        Review::Unknown => {}
    }
    if facts.mergeable == Some(false) {
        parts.push("not mergeable".into());
    }
    if let Some(error) = &facts.forge_error {
        parts.push(format!("forge error: {}", one_line(error)));
    }
    parts.push(session(worker));
    if worker.archived {
        parts.push("archived".into());
    }
    parts.join(" · ")
}

fn session(worker: &Worker) -> String {
    match (&worker.facts.session, worker.facts.exit_code) {
        (SessionState::Exited, Some(code)) => format!("exited with code {code}"),
        (SessionState::Exited, None) => "exited".into(),
        (SessionState::Lost, _) => "session lost".into(),
        (SessionState::NeedsInput, _) => "waiting for input".into(),
        (SessionState::Running | SessionState::Idle, _) => "still running".into(),
    }
}

fn pr_number(url: Option<&str>) -> Option<&str> {
    let number = url?.trim_end_matches('/').rsplit('/').next()?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

fn totals(files: &[FileChange]) -> (u64, u64) {
    files.iter().fold((0, 0), |(a, r), f| {
        (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
    })
}

/// Group paths by their top two directories (`crates/sigmadock-ui`), largest first.
fn group(files: &[FileChange]) -> Vec<(String, usize, u64, u64)> {
    let mut groups: Vec<(String, usize, u64, u64)> = Vec::new();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        let name = match parts.len() {
            1 => "(root)".to_owned(),
            2 => parts[0].to_owned(),
            _ => format!("{}/{}", parts[0], parts[1]),
        };
        let (added, removed) = (file.added.unwrap_or(0), file.removed.unwrap_or(0));
        match groups.iter_mut().find(|g| g.0 == name) {
            Some(g) => {
                g.1 += 1;
                g.2 += added;
                g.3 += removed;
            }
            None => groups.push((name, 1, added, removed)),
        }
    }
    groups.sort_by(|a, b| (b.2 + b.3).cmp(&(a.2 + a.3)).then_with(|| a.0.cmp(&b.0)));
    groups
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A JSON string is a valid YAML double-quoted scalar.
fn quoted(text: &str) -> String {
    serde_json::to_string(&one_line(text)).unwrap_or_else(|_| "\"\"".into())
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if c == ' ' {
            out.push('-');
        } else if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn duration(seconds: u64) -> String {
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    match (hours, minutes) {
        (0, 0) => "<1m".into(),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// UTC ISO-8601 minute timestamp, e.g. `2026-10-07T14:03Z`.
fn timestamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Facts;

    fn worker() -> Worker {
        serde_json::from_value(serde_json::json!({
            "id": "w", "project_id": "p", "title": "Fix the\nlogin bug", "agent": "claude",
            "branch": "sigma/w", "worktree": "/tmp/w", "port": 4200, "created_at": 1_791_381_780,
            "archived": false, "facts": Facts::default(),
        }))
        .unwrap()
    }

    #[test]
    fn timestamps_are_utc_iso() {
        assert_eq!(timestamp(0), "1970-01-01T00:00Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00Z");
        assert_eq!(timestamp(1_791_381_780), "2026-10-07T14:03Z");
    }

    #[test]
    fn summary_restates_facts_commits_and_files() {
        let mut worker = worker();
        worker.prompt = Some("Fix it\n\nand run tests".into());
        worker.finished_at = Some(worker.created_at + 4320);
        worker.facts.session = SessionState::Exited;
        worker.facts.exit_code = Some(0);
        worker.facts.pr = PullRequestState::Open;
        worker.facts.pr_url = Some("https://github.com/o/r/pull/42".into());
        worker.facts.checks = Checks::Passed;
        worker.facts.review = Review::Approved;
        worker.facts.mergeable = Some(true);
        let changes = Changes {
            commits: vec!["Validate token".into(), "Add regression test".into()],
            files: vec![
                FileChange {
                    path: "crates/auth/src/lib.rs".into(),
                    added: Some(80),
                    removed: Some(10),
                },
                FileChange {
                    path: "crates/auth/Cargo.toml".into(),
                    added: Some(4),
                    removed: Some(2),
                },
                FileChange {
                    path: "README.md".into(),
                    added: Some(1),
                    removed: Some(0),
                },
                FileChange {
                    path: "logo.png".into(),
                    added: None,
                    removed: None,
                },
            ],
            ..Default::default()
        };
        let text = markdown(&worker, "my \"repo\"", &changes, 0);
        assert!(
            text.starts_with("---\ntitle: \"Fix the login bug\"\nproject: \"my \\\"repo\\\"\"\n"),
            "{text}"
        );
        assert!(text.contains("status: ready-to-merge\n"), "{text}");
        assert!(text.contains("review: approved\n"));
        assert!(text.contains("checks: passed\n"));
        assert!(text.contains("duration: 1h 12m\n"));
        assert!(text.contains("# Fix the login bug\n"));
        assert!(text.contains(
            "**Outcome:** Ready to merge · PR #42 open · checks passed · approved · exited with code 0\n"
        ), "{text}");
        assert!(text.contains("> Fix it\n>\n> and run tests\n"), "{text}");
        assert!(text.contains("- Validate token\n- Add regression test\n"));
        assert!(text.contains("4 files changed, +85 −12"), "{text}");
        assert!(
            text.contains("- `crates/auth` — 2 files, +84 −12\n- `(root)` — 2 files, +1 −0\n"),
            "{text}"
        );
    }

    #[test]
    fn summary_reports_missing_work_plainly() {
        let mut worker = worker();
        worker.facts.session = SessionState::Lost;
        let changes = Changes {
            uncommitted: true,
            note: Some("Branch no longer exists.".into()),
            ..Default::default()
        };
        let text = markdown(&worker, "repo", &changes, 0);
        assert!(text.contains("_No commits on this branch._"));
        assert!(text.contains("_Uncommitted changes remain in the worktree._"));
        assert!(text.contains("_Branch no longer exists._"));
        assert!(text.contains("Needs you · no PR · session lost"), "{text}");
        assert!(
            !text.contains("## Task") && !text.contains("## Files") && !text.contains("duration:")
        );
    }
}
