use super::*;
use sigmadock_core::{CiEntry, CiPreview, unix_time};
const MAX_ENTRIES: usize = 200;
fn state(value: &Value) -> String {
    let result = value["conclusion"]
        .as_str()
        .filter(|value| !value.is_empty())
        .or_else(|| value["status"].as_str())
        .or_else(|| value["state"].as_str())
        .unwrap_or("unknown");
    match result {
        "success" => "passed",
        "failure" | "failed" | "error" | "timed_out" | "action_required" | "startup_failure" => {
            "failed"
        }
        "cancelled" | "canceled" => "cancelled",
        "skipped" | "neutral" => "skipped",
        "in_progress" | "running" => "running",
        "queued" | "waiting" | "requested" | "pending" => "pending",
        _ => "unknown",
    }
    .into()
}
fn entry(value: &Value, kind: &str, fallback: usize) -> CiEntry {
    let name = value["name"]
        .as_str()
        .or_else(|| value["context"].as_str())
        .unwrap_or("Unnamed result");
    let details = format!(
        "{}\n{}\n{}",
        value["description"].as_str().unwrap_or(""),
        value["output"]["summary"].as_str().unwrap_or(""),
        value["output"]["text"].as_str().unwrap_or("")
    );
    CiEntry {
        id: format!("{kind}-{}", value["id"].as_u64().unwrap_or(fallback as u64)),
        kind: kind.into(),
        name: task_text(name, 256),
        state: state(value),
        url: value["html_url"]
            .as_str()
            .or_else(|| value["target_url"].as_str())
            .filter(|url| {
                Url::parse(url).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none()
                })
            })
            .map(str::to_owned),
        truncated: details.len() > 8192,
        details: task_text(&details, 8192),
    }
}
impl RestForge {
    fn head(&self, branch: &str) -> Result<String> {
        let sha = if let Some(pull) = self.pull(branch)? {
            pull["head"]["sha"]
                .as_str()
                .context("missing PR head")?
                .to_owned()
        } else {
            let branch =
                percent_encoding::utf8_percent_encode(branch, percent_encoding::NON_ALPHANUMERIC);
            let value = self.get(&format!("{}/branches/{branch}", self.prefix()), &[])?;
            value["commit"]["sha"]
                .as_str()
                .or_else(|| value["commit"]["id"].as_str())
                .context("missing branch head")?
                .to_owned()
        };
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid forge commit identifier");
        }
        Ok(sha)
    }
    pub(super) fn preview(&self, branch: &str) -> Result<CiPreview> {
        let sha = self.head(branch)?;
        let mut report = CiPreview {
            complete: true,
            head_sha: sha.clone(),
            current_head: sha.clone(),
            refreshed_at: unix_time(),
            entries: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
        };
        if self.config.kind == "github" {
            for (index, check) in self
                .checks(&sha)?
                .iter()
                .filter(|check| check["head_sha"].as_str().is_none_or(|head| head == sha))
                .enumerate()
                .take(MAX_ENTRIES + 1)
            {
                if report.entries.len() == MAX_ENTRIES {
                    report.truncated = true;
                    break;
                }
                let mut detail = entry(check, "check", index);
                if detail.state == "failed"
                    && let Some(id) = check["id"].as_u64()
                {
                    match self.pages(
                        &format!("{}/check-runs/{id}/annotations", self.prefix()),
                        None,
                        &[],
                    ) {
                        Ok(annotations) => {
                            for annotation in annotations {
                                detail.details.push_str(&format!(
                                    "\n{}:{} — {}",
                                    annotation["path"].as_str().unwrap_or("general"),
                                    annotation["start_line"],
                                    annotation["message"].as_str().unwrap_or("")
                                ));
                            }
                            detail.truncated |= detail.details.len() > 8192;
                            detail.details = task_text(&detail.details, 8192);
                        }
                        Err(error) => detail
                            .details
                            .push_str(&format!("\nAnnotations unavailable: {error}")),
                    }
                }
                detail.truncated |= detail.details.len() > 8192;
                detail.details = task_text(&detail.details, 8192);
                report.entries.push(detail);
            }
            report.warnings.push("GitHub full job logs are not downloaded because their redirects leave the configured API. Check output, annotations and job steps are shown where available.".into());
        }
        match self.get(&format!("{}/commits/{sha}/status", self.prefix()), &[]) {
            Ok(status) => {
                if let Some(checks) = status["statuses"].as_array() {
                    for (index, check) in checks.iter().enumerate() {
                        if report.entries.len() == MAX_ENTRIES {
                            report.truncated = true;
                            break;
                        }
                        report.entries.push(entry(check, "status", index));
                    }
                } else {
                    report.complete = false;
                    report
                        .warnings
                        .push("Invalid commit statuses response".into());
                }
            }
            Err(error) => {
                report.complete = false;
                report
                    .warnings
                    .push(format!("Commit status endpoint unavailable: {error}"));
            }
        }
        if self.config.kind == "github" || self.config.actions {
            let runs = if self.config.kind == "github" {
                self.pages(
                    &format!("{}/actions/runs", self.prefix()),
                    Some("workflow_runs"),
                    &[("head_sha", &sha)],
                )
                .map(|runs| {
                    let mut latest: HashMap<(String, String), Value> = HashMap::new();
                    for run in runs
                        .into_iter()
                        .filter(|run| run["head_sha"].as_str() == Some(&sha))
                    {
                        let key = (run["workflow_id"].to_string(), run["event"].to_string());
                        if latest
                            .get(&key)
                            .is_none_or(|previous| run["id"].as_u64() > previous["id"].as_u64())
                        {
                            latest.insert(key, run);
                        }
                    }
                    let mut runs: Vec<_> = latest.into_values().collect();
                    runs.sort_by_key(|run| run["id"].as_u64());
                    runs
                })
            } else {
                self.action_runs(&sha)
            };
            match runs {
                Ok(runs) => {
                    for (index, run) in runs.iter().enumerate() {
                        if report.entries.len() == MAX_ENTRIES {
                            report.truncated = true;
                            break;
                        }
                        report.entries.push(entry(run, "workflow", index));
                        let Some(id) = run["id"].as_u64() else {
                            report.complete = false;
                            report
                                .warnings
                                .push("Workflow has no run identifier; jobs unavailable.".into());
                            continue;
                        };
                        let jobs = if self.config.kind == "github" {
                            self.pages(
                                &format!("{}/actions/runs/{id}/jobs", self.prefix()),
                                Some("jobs"),
                                &[("filter", "latest")],
                            )
                        } else {
                            self.get(&format!("{}/actions/runs/{id}/jobs", self.prefix()), &[])
                                .and_then(|value| {
                                    value
                                        .as_array()
                                        .cloned()
                                        .context("invalid Forgejo jobs response")
                                })
                        };
                        match jobs {
                            Ok(jobs) => {
                                for (index, job) in jobs
                                    .iter()
                                    .filter(|job| {
                                        job["head_sha"].as_str().is_none_or(|head| head == sha)
                                    })
                                    .enumerate()
                                {
                                    if report.entries.len() == MAX_ENTRIES {
                                        report.truncated = true;
                                        break;
                                    }
                                    let mut detail = entry(job, "job", index);
                                    if let Some(steps) = job["steps"].as_array() {
                                        for step in steps {
                                            detail.details.push_str(&format!(
                                                "\n{}: {}",
                                                step["name"].as_str().unwrap_or("Step"),
                                                state(step)
                                            ));
                                        }
                                    }
                                    if self.config.kind == "forgejo"
                                        && detail.state == "failed"
                                        && let Some(id) = job["id"].as_u64()
                                    {
                                        match self.job_log(id, job["attempt"].as_u64().unwrap_or(1))
                                        {
                                            Ok(log) => {
                                                detail.details.push_str("\nBounded job log excerpt (at most 32 KiB fetched):\n");
                                                detail.details.push_str(&log);
                                            }
                                            Err(error) => detail.details.push_str(&format!(
                                                "\nJob log unavailable: {error}"
                                            )),
                                        }
                                    }
                                    detail.truncated |= detail.details.len() > 8192;
                                    detail.details = task_text(&detail.details, 8192);
                                    report.entries.push(detail);
                                }
                            }
                            Err(error) => {
                                report.complete = false;
                                report
                                    .warnings
                                    .push(format!("Workflow {id} jobs unavailable: {error}"));
                            }
                        }
                    }
                }
                Err(error) => {
                    report.complete = false;
                    report
                        .warnings
                        .push(format!("Workflow endpoint unavailable: {error}"));
                }
            }
        } else {
            report.warnings.push("Forgejo Actions endpoints are disabled in this worker’s forge configuration; commit statuses are shown.".into());
        }
        report.current_head = self.head(branch)?;
        if report.current_head != report.head_sha {
            report.warnings.push("The branch/PR head changed during refresh. These results are stale; refresh before sending feedback.".into());
        }
        Ok(report)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_states_cover_active_terminal_and_unknown_outcomes() {
        for (value, expected) in [
            ("queued", "pending"),
            ("in_progress", "running"),
            ("success", "passed"),
            ("timed_out", "failed"),
            ("cancelled", "cancelled"),
            ("skipped", "skipped"),
            ("neutral", "skipped"),
            ("completed", "unknown"),
        ] {
            assert_eq!(
                state(&serde_json::json!({"status":"completed","conclusion":value})),
                expected
            );
        }
    }
    #[test]
    fn details_are_bounded_and_links_only_open_http_urls_without_credentials() {
        let result = entry(
            &serde_json::json!({"id":1,"name":"test","html_url":"javascript:alert(1)","output":{"summary":"x".repeat(9000)}}),
            "check",
            0,
        );
        assert!(result.truncated && result.details.len() <= 8192 && result.url.is_none());
    }
}

#[cfg(test)]
mod incomplete_ci_tests {
    use crate::{
        Forge,
        tests::{local_forge, mock},
    };
    #[test]
    fn passing_entries_do_not_hide_failed_provider_endpoints() {
        let sha = "a".repeat(40);
        let pull = format!(r#"{{"number":1,"head":{{"sha":"{sha}"}}}}"#);
        let (url, server) = mock(vec![
            (
                "200 OK",
                "",
                r#"[{"number":1,"head":{"ref":"sigma/task"}}]"#,
            ),
            ("200 OK", "", &pull),
            (
                "200 OK",
                "",
                r#"{"statuses":[{"context":"tests","state":"success"}]}"#,
            ),
            ("403 Forbidden", "", "{}"),
            (
                "200 OK",
                "",
                r#"[{"number":1,"head":{"ref":"sigma/task"}}]"#,
            ),
            ("200 OK", "", &pull),
        ]);
        let mut forge = local_forge(url);
        forge.config.actions = true;
        let report = forge.ci_preview("sigma/task").unwrap();
        assert_eq!(report.entries[0].state, "passed");
        assert!(!report.complete);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("unavailable"))
        );
        server.join().unwrap();
    }
}
