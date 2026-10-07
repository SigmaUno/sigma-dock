//! Worktree operations deliberately refuse to discard dirty files.
use anyhow::{Context, Result, bail};
use sigmadock_core::summary::{Changes, FileChange, MAX_COMMITS};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(git_raw(repo, args)?.trim().into())
}
pub fn root(path: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?).canonicalize()?)
}
/// Detect origin's default branch without consulting the checkout's HEAD.
pub fn default_base(repo: &Path) -> Result<String> {
    let reference = git(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .context("origin's default branch is unknown; configure a project base branch or pass an explicit base ref")?;
    let branch = reference
        .strip_prefix("refs/remotes/origin/")
        .context("origin/HEAD does not point to an origin branch")?;
    validate_base_branch(repo, branch)?;
    Ok(branch.into())
}

pub fn validate_base_branch(repo: &Path, branch: &str) -> Result<()> {
    if branch.is_empty() || branch.starts_with('-') || branch == "HEAD" {
        bail!("invalid project base branch");
    }
    git(repo, &["check-ref-format", &format!("refs/heads/{branch}")])?;
    Ok(())
}

/// Update only the selected remote-tracking ref, with a bounded network wait.
/// A failed fetch may use a cached commit, but never the local checkout's HEAD.
pub fn fresh_base(repo: &Path, branch: &str) -> Result<(String, Option<String>)> {
    fresh_base_with_timeout(repo, branch, std::time::Duration::from_secs(8))
}

fn fresh_base_with_timeout(
    repo: &Path,
    branch: &str,
    timeout: std::time::Duration,
) -> Result<(String, Option<String>)> {
    use std::{io::Read, os::unix::process::CommandExt, process::Stdio, time::Instant};
    validate_base_branch(repo, branch)?;
    let reference = format!("refs/remotes/origin/{branch}");
    let refspec = format!("+refs/heads/{branch}:{reference}");
    let fetch = (|| -> Result<()> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                &refspec,
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("run git fetch")?;
        let mut stderr = child.stderr.take().context("fetch stderr")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.by_ref().take(8192).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            String::from_utf8_lossy(&bytes).trim().to_owned()
        });
        let started = Instant::now();
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if started.elapsed() < timeout => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                result => {
                    // SAFETY: this child owns a new process group; kill its transport too.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.wait();
                    break match result {
                        Err(error) => Err(anyhow::Error::from(error)),
                        _ => Err(anyhow::anyhow!("fetch timed out")),
                    };
                }
            }
        };
        let message = reader.join().unwrap_or_default();
        if !outcome?.success() {
            bail!("fetch failed: {message}");
        }
        Ok(())
    })();
    let warning = fetch.err().map(|error| {
        format!("Could not refresh origin/{branch}; using the cached remote base. {error}")
    });
    // Freeze the exact commit so another fetch cannot change it before worktree creation.
    let commit = git(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
    .with_context(|| {
        format!("no cached origin/{branch} commit; fetch the branch or pass an explicit base ref")
    })?;
    Ok((commit, warning))
}
pub fn create(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    git(repo, &["check-ref-format", "--branch", branch])?;
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            "--",
            path.to_str().context("non-UTF8 worktree path")?,
            base,
        ],
    )?;
    Ok(())
}
pub fn remove(repo: &Path, path: &Path) -> Result<()> {
    git(
        repo,
        &[
            "worktree",
            "remove",
            path.to_str().context("non-UTF8 path")?,
        ],
    )?;
    Ok(())
}
pub fn prune(repo: &Path) -> Result<String> {
    git(repo, &["worktree", "prune", "--verbose"])
}
mod diff;
mod fork;
pub use diff::{diff_report, recorded_base};
pub use fork::{
    apply_fork_snapshot, fork_snapshot, release_fork_snapshot, rollback_unstarted_fork,
};
/// Inspect local refs and files only; the checks pane never fetches implicitly.
pub fn readiness(
    path: &Path,
    base_branch: Option<&str>,
    worker_branch: &str,
) -> Result<sigmadock_core::GitReadiness> {
    let head = git(path, &["rev-parse", "HEAD"])?;
    let raw = git_raw(path, &["status", "--porcelain=v1", "-z"])?;
    let mut dirty = Vec::new();
    let mut records = raw.split('\0').filter(|line| !line.is_empty());
    while let Some(record) = records.next() {
        dirty.push(sigmadock_core::task_text(record, 1024));
        if record.starts_with('R')
            || record.starts_with('C')
            || record.get(1..2).is_some_and(|s| s == "R" || s == "C")
        {
            let _ = records.next();
        }
    }
    let dirty_truncated = dirty.len() > 200;
    dirty.truncate(200);
    let base = base_branch.map(|branch| format!("refs/remotes/origin/{branch}"));
    let counts = base.as_ref().and_then(|base| {
        let output = git(
            path,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{base}...HEAD"),
            ],
        )
        .ok()?;
        let mut parts = output.split_whitespace();
        Some((
            parts.next()?.parse::<u64>().ok()?,
            parts.next()?.parse::<u64>().ok()?,
        ))
    });
    let upstream = git(path, &["rev-parse", "--symbolic-full-name", "@{upstream}"])
        .ok()
        .unwrap_or_else(|| format!("refs/remotes/origin/{worker_branch}"));
    let unpushed = match git(path, &["rev-parse", "--verify", &upstream]) {
        Ok(_) => git(path, &["rev-list", "--count", &format!("{upstream}..HEAD")])
            .ok()
            .and_then(|s| s.parse().ok()),
        Err(_) => counts.map(|(_, ahead)| ahead),
    };
    Ok(sigmadock_core::GitReadiness {
        head,
        base,
        dirty,
        dirty_truncated,
        ahead: counts.map(|(_, ahead)| ahead),
        behind: counts.map(|(behind, _)| behind),
        unpushed,
    })
}
/// Largest patch returned to clients, leaving room for JSON escaping in a 4 MiB frame.
/// The rest is cut at a line boundary.
pub const PATCH_LIMIT: usize = 256 * 1024;
/// Commit the worktree branch started from, read from the branch's creation reflog entry.
/// Falls back to `HEAD` when the reflog has expired, so only uncommitted work is shown.
pub fn fork_point(path: &Path) -> String {
    git(path, &["symbolic-ref", "-q", "HEAD"])
        .and_then(|branch| git(path, &["reflog", "show", "--format=%H", &branch, "--"]))
        .ok()
        .and_then(|log| log.lines().last().map(str::to_owned))
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "HEAD".into())
}
/// Legacy callers can still load a patch, using origin's default branch when known.
pub fn patch(path: &Path) -> Result<(String, bool)> {
    let base = default_base(path)
        .map(|branch| format!("refs/remotes/origin/{branch}"))
        .unwrap_or_else(|_| fork_point(path));
    if base == "HEAD" {
        bail!("worker base is unknown; configure a project base branch");
    }
    let report = diff_report(path, &base)?;
    Ok((report.text(false), report.truncated))
}
fn git_raw(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!("git: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
/// URL of the `origin` remote.
pub fn remote_url(repo: &Path) -> Result<String> {
    git(repo, &["remote", "get-url", "origin"])
}
pub fn clean(path: &Path) -> Result<bool> {
    Ok(git(path, &["status", "--porcelain"])?.is_empty())
}
/// Most changed paths read for a summary; untracked files are counted individually.
const SUMMARY_FILES: usize = 500;
/// Commit subjects and diff stats for a worker branch since it forked. Reads the live
/// worktree when it exists, so uncommitted edits count, and the branch otherwise.
pub fn changes(repo: &Path, worktree: &Path, branch: &str) -> Changes {
    let reference = format!("refs/heads/{branch}");
    if git(repo, &["rev-parse", "--verify", "-q", &reference]).is_err() {
        return Changes {
            note: Some(format!(
                "Branch {branch} no longer exists; git history is unavailable."
            )),
            ..Default::default()
        };
    }
    // The branch's creation reflog entry is its fork point; the merge base is a fallback
    // once the reflog has expired.
    let base = git(repo, &["reflog", "show", "--format=%H", &reference, "--"])
        .ok()
        .and_then(|log| log.lines().last().map(str::to_owned))
        .filter(|sha| !sha.is_empty())
        .or_else(|| git(repo, &["merge-base", "HEAD", &reference]).ok());
    let Some(base) = base else {
        return Changes {
            note: Some(
                "Could not find where the branch started; git history is unavailable.".into(),
            ),
            ..Default::default()
        };
    };
    let mut changes = Changes::default();
    if let Ok(log) = git(
        repo,
        &[
            "log",
            "--reverse",
            "--format=%s",
            &format!("{base}..{reference}"),
        ],
    ) {
        let subjects: Vec<String> = log.lines().map(str::to_owned).collect();
        changes.more_commits = subjects.len().saturating_sub(MAX_COMMITS);
        changes.commits = subjects.into_iter().skip(changes.more_commits).collect();
    }
    let live = worktree.exists();
    let numstat = if live {
        git(
            worktree,
            &["diff", "--numstat", "-z", "--find-renames", &base],
        )
    } else {
        git(
            repo,
            &[
                "diff",
                "--numstat",
                "-z",
                "--find-renames",
                &base,
                &reference,
            ],
        )
    };
    match numstat {
        Ok(text) => changes.files = parse_numstat(&text),
        Err(error) => changes.note = Some(format!("Diff stats unavailable: {error}")),
    }
    if live {
        changes.uncommitted = !clean(worktree).unwrap_or(true);
        let untracked = git(
            worktree,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )
        .unwrap_or_default();
        for file in untracked.split('\0').filter(|file| !file.is_empty()) {
            if changes.files.len() >= SUMMARY_FILES {
                break;
            }
            // `--no-index` exits 1 when files differ, which is the expected case here.
            let stat = Command::new("git")
                .arg("-C")
                .arg(worktree)
                .args(["diff", "--numstat", "--no-index", "--", "/dev/null", file])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default();
            let mut fields = stat.split('\t');
            changes.files.push(FileChange {
                path: file.to_owned(),
                added: fields.next().and_then(|n| n.parse().ok()),
                removed: fields.next().and_then(|n| n.parse().ok()),
            });
        }
    }
    changes.files.truncate(SUMMARY_FILES);
    changes
}
/// Parse `git diff --numstat -z`, where a rename has an empty path followed by old and new.
fn parse_numstat(text: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            let _old = fields.next();
            fields.next().unwrap_or_default()
        } else {
            path
        };
        files.push(FileChange {
            path: path.to_owned(),
            added: added.parse().ok(),
            removed: removed.parse().ok(),
        });
    }
    files
}
pub fn rollback(repo: &Path, path: &Path, branch: &str) {
    if remove(repo, path).is_ok() {
        let _ = git(repo, &["branch", "-d", branch]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // Parallel tests can read the same clock value; the counter keeps paths unique.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "sigmadock-base-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-b", "trunk", "seed"]).unwrap();
            let seed = path.join("seed");
            git(&seed, &["config", "user.email", "test@localhost"]).unwrap();
            git(&seed, &["config", "user.name", "Test"]).unwrap();
            git(
                &seed,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ],
            )
            .unwrap();
            git(&path, &["clone", "--bare", "seed", "origin.git"]).unwrap();
            git(&path, &["clone", "origin.git", "checkout"]).unwrap();
            Self(path)
        }
        fn repo(&self) -> PathBuf {
            self.0.join("checkout")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fork_snapshot_preserves_source_and_index_layers() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(repo.join("tracked"), "base\n").unwrap();
        std::fs::write(repo.join("deleted"), "delete me\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "base"],
        )
        .unwrap();
        std::fs::write(repo.join("tracked"), "staged\n").unwrap();
        std::fs::write(repo.join("ignored"), "explicitly staged\n").unwrap();
        git(&repo, &["add", "tracked"]).unwrap();
        git(&repo, &["add", "-f", "ignored"]).unwrap();
        std::fs::write(repo.join("tracked"), "working\n").unwrap();
        std::fs::remove_file(repo.join("deleted")).unwrap();
        std::fs::write(repo.join("new file"), [0, 255, 1, 0]).unwrap();
        std::os::unix::fs::symlink("tracked", repo.join("link")).unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let status = git(&repo, &["status", "--porcelain"]).unwrap();
        let index_path = repo.join(git(&repo, &["rev-parse", "--git-path", "index"]).unwrap());
        let before = std::fs::read(index_path.clone()).unwrap();
        let staged = git(&repo, &["diff", "--cached", "--binary"]).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = fork_snapshot(&repo, "trunk", true, &id).unwrap();
        let dest = fixture.0.join("fork");
        create(&repo, &dest, "fork", &snapshot.head).unwrap();
        apply_fork_snapshot(&dest, &snapshot).unwrap();
        assert_eq!(git(&dest, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(git(&dest, &["status", "--porcelain"]).unwrap(), status);
        assert_eq!(
            git(&dest, &["diff", "--cached", "--binary"]).unwrap(),
            staged
        );
        assert_eq!(std::fs::read(dest.join("tracked")).unwrap(), b"working\n");
        assert_eq!(
            std::fs::read(dest.join("new file")).unwrap(),
            [0, 255, 1, 0]
        );
        assert_eq!(
            std::fs::read_link(dest.join("link")).unwrap(),
            PathBuf::from("tracked")
        );
        assert_eq!(std::fs::read(index_path).unwrap(), before);
        assert_eq!(git(&repo, &["status", "--porcelain"]).unwrap(), status);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), head);
        release_fork_snapshot(&repo, &id).unwrap();
        assert!(
            git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/sigmadock/forks/{id}")
                ]
            )
            .is_err()
        );
        rollback_unstarted_fork(&repo, &dest, "fork", &snapshot.head).unwrap();
        assert!(!dest.exists());
    }

    #[test]
    fn head_only_fork_ignores_dirty_files_and_snapshot_survives_source_advance() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::write(repo.join("local"), "request-time").unwrap();
        let clean = fork_snapshot(&repo, "trunk", false, &id).unwrap();
        let dirty = fork_snapshot(&repo, "trunk", true, &id).unwrap();
        std::fs::write(repo.join("local"), "later").unwrap();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "later"],
        )
        .unwrap();
        let dest = fixture.0.join("fork");
        create(&repo, &dest, "fork", &clean.head).unwrap();
        apply_fork_snapshot(&dest, &clean).unwrap();
        assert!(!dest.join("local").exists());
        apply_fork_snapshot(&dest, &dirty).unwrap();
        assert_eq!(std::fs::read(dest.join("local")).unwrap(), b"request-time");
        git(
            &dest,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "advanced",
            ],
        )
        .unwrap();
        assert!(rollback_unstarted_fork(&repo, &dest, "fork", &clean.head).is_err());
        assert!(dest.exists());
        release_fork_snapshot(&repo, &id).unwrap();
    }

    #[test]
    fn fetch_timeout_uses_cache_and_missing_cache_fails() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        assert_eq!(default_base(&repo).unwrap(), "trunk");
        let cached = git(&repo, &["rev-parse", "origin/trunk"]).unwrap();
        let transport = fixture.0.join("slow-upload-pack");
        std::fs::write(&transport, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&transport, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &repo,
            &[
                "config",
                "remote.origin.uploadpack",
                transport.to_str().unwrap(),
            ],
        )
        .unwrap();
        let before = Instant::now();
        let (base, warning) =
            fresh_base_with_timeout(&repo, "trunk", Duration::from_millis(100)).unwrap();
        assert!(before.elapsed() < Duration::from_secs(3));
        assert_eq!(base, cached);
        assert!(warning.unwrap().contains("timed out"));
        assert!(fresh_base_with_timeout(&repo, "missing", Duration::from_millis(100)).is_err());
        for branch in ["HEAD", "-bad", "bad:ref", "../bad", ""] {
            assert!(validate_base_branch(&repo, branch).is_err());
        }
    }

    #[test]
    fn readiness_observes_dirty_ahead_behind_and_push_state_without_fetching() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        git(&repo, &["checkout", "-b", "sigma/test"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "worker",
            ],
        )
        .unwrap();
        let seed = fixture.0.join("seed");
        git(
            &seed,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        )
        .unwrap();
        let origin = fixture.0.join("origin.git");
        git(&seed, &["push", origin.to_str().unwrap(), "trunk"]).unwrap();
        git(&repo, &["fetch", "origin"]).unwrap();
        std::fs::write(repo.join("dirty file"), "preserve").unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let report = readiness(&repo, Some("trunk"), "sigma/test").unwrap();
        assert_eq!(report.behind, Some(1));
        assert_eq!(report.ahead, Some(1));
        assert_eq!(report.unpushed, Some(1));
        assert_eq!(report.dirty, vec!["?? dirty file"]);
        git(&repo, &["push", "-u", "origin", "sigma/test"]).unwrap();
        assert_eq!(
            readiness(&repo, Some("trunk"), "sigma/test")
                .unwrap()
                .unpushed,
            Some(0)
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(repo.join("dirty file")).unwrap(),
            "preserve"
        );
        assert_eq!(
            readiness(&repo, Some("missing"), "sigma/test")
                .unwrap()
                .behind,
            None
        );
    }

    #[test]
    fn local_only_repo_requires_explicit_base() {
        let fixture = Fixture::new();
        let seed = fixture.0.join("seed");
        assert!(default_base(&seed).is_err());
        assert!(fresh_base(&seed, "trunk").is_err());
        create(
            &seed,
            &fixture.0.join("explicit"),
            "sigma/explicit",
            "trunk",
        )
        .unwrap();
    }

    #[test]
    fn changes_list_commits_and_stats_live_and_after_cleanup() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        let commit = |dir: &Path, message: &str| {
            git(dir, &["add", "-A"]).unwrap();
            git(
                dir,
                &["-c", "commit.gpgsign=false", "commit", "-qm", message],
            )
            .unwrap();
        };
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        commit(&repo, "base");
        let worktree = fixture.0.join("wt");
        create(&repo, &worktree, "sigma/test", "HEAD").unwrap();
        std::fs::write(worktree.join("a.txt"), "one\ntwo\n").unwrap();
        commit(&worktree, "Add two");
        git(&worktree, &["mv", "a.txt", "b.txt"]).unwrap();
        commit(&worktree, "Rename a to b");
        std::fs::write(worktree.join("new.txt"), "x\ny\n").unwrap();
        // Later work on the main checkout is not part of the worker's session.
        std::fs::write(repo.join("main.txt"), "main\n").unwrap();
        commit(&repo, "main work");

        let live = changes(&repo, &worktree, "sigma/test");
        assert_eq!(live.commits, ["Add two", "Rename a to b"]);
        assert!(live.uncommitted && live.note.is_none());
        let mut paths: Vec<_> = live
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.added))
            .collect();
        paths.sort();
        assert_eq!(paths, [("b.txt", Some(1)), ("new.txt", Some(2))]);

        std::fs::remove_file(worktree.join("new.txt")).unwrap();
        remove(&repo, &worktree).unwrap();
        let archived = changes(&repo, &worktree, "sigma/test");
        assert_eq!(archived.commits, live.commits);
        assert!(!archived.uncommitted);
        assert_eq!(archived.files.len(), 1);
        assert_eq!(archived.files[0].path, "b.txt");

        let gone = changes(&repo, &worktree, "sigma/missing");
        assert!(gone.commits.is_empty() && gone.note.is_some());
    }

    #[test]
    fn diff_uses_merge_base_and_separates_committed_local_and_untracked() {
        use sigmadock_core::diff::DiffSectionKind;
        let fixture = Fixture::new();
        let repo = fixture.repo();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(repo.join("a.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-qm", "base"]).unwrap();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(recorded_base(&repo, "HEAD").unwrap(), base);
        assert_eq!(recorded_base(&repo, "trunk").unwrap(), "refs/heads/trunk");
        let worker = fixture.0.join("diff-worker");
        create(&repo, &worker, "sigma/diff", "trunk").unwrap();
        std::fs::write(worker.join("a.txt"), "base\ncommitted\n").unwrap();
        git(&worker, &["commit", "-qam", "worker commit"]).unwrap();
        // Advancing the base must not add unrelated base changes to the worker diff.
        std::fs::write(repo.join("unrelated"), "base only\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-qm", "base advanced"]).unwrap();
        let clean_report = diff_report(&worker, "refs/heads/trunk").unwrap();
        assert_eq!(clean_report.merge_base, base);
        assert_eq!(clean_report.sections[0].files.len(), 1);
        assert!(clean_report.text(false).contains("+committed"));
        assert!(clean_report.sections[1].files.is_empty());
        std::fs::write(worker.join("a.txt"), "base\ncommitted\nstaged\n").unwrap();
        git(&worker, &["add", "a.txt"]).unwrap();
        std::fs::write(worker.join("a.txt"), "base\ncommitted\nstaged\nunstaged\n").unwrap();
        std::fs::write(worker.join("new name\t.txt"), "untracked\n").unwrap();
        std::fs::write(worker.join("binary.dat"), [0, 1, 2, 0]).unwrap();
        let report = diff_report(&worker, "refs/heads/trunk").unwrap();
        assert_eq!(report.sections[1].kind, DiffSectionKind::Uncommitted);
        assert!(report.sections[1].files[0].patch.contains("+staged"));
        assert!(report.sections[1].files[0].patch.contains("+unstaged"));
        let new = report.sections[2]
            .files
            .iter()
            .find(|f| f.path == "new name\t.txt")
            .unwrap();
        assert_eq!(new.added, Some(1));
        let previous = new.blob_id.clone();
        assert!(
            report.sections[2]
                .files
                .iter()
                .find(|f| f.path == "binary.dat")
                .unwrap()
                .binary
        );
        std::fs::write(worker.join("new name\t.txt"), "changed\n").unwrap();
        let changed = diff_report(&worker, "refs/heads/trunk").unwrap();
        assert_ne!(
            previous,
            changed.sections[2]
                .files
                .iter()
                .find(|f| f.path == "new name\t.txt")
                .unwrap()
                .blob_id
        );
        assert!(report.text(true).contains("M a.txt | +1 -0"));
        assert!(!report.text(true).contains("@@"));
        assert!(diff_report(&worker, "refs/heads/missing").is_err());
        assert!(clean(&repo).unwrap());
    }

    #[test]
    fn diff_bounds_patches_and_keeps_rename_and_delete_metadata() {
        let fixture = Fixture::new();
        let repo = fixture.repo();
        git(&repo, &["config", "user.email", "test@localhost"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(repo.join("old name.txt"), "rename me\n").unwrap();
        std::fs::write(repo.join("delete.txt"), "delete me\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-qm", "base"]).unwrap();
        let base = recorded_base(&repo, "HEAD").unwrap();
        git(&repo, &["mv", "old name.txt", "new name.txt"]).unwrap();
        git(&repo, &["rm", "delete.txt"]).unwrap();
        let report = diff_report(&repo, &base).unwrap();
        assert!(
            report.sections[1]
                .files
                .iter()
                .any(|f| f.path == "new name.txt" && f.status == "R")
        );
        assert!(
            report.sections[1]
                .files
                .iter()
                .any(|f| f.path == "delete.txt" && f.status == "D")
        );
        for i in 0..6 {
            std::fs::write(
                repo.join(format!("huge{i}")),
                "a long added line\n".repeat(10000),
            )
            .unwrap();
        }
        let report = diff_report(&repo, &base).unwrap();
        assert!(report.truncated && !report.warnings.is_empty());
        let files: Vec<_> = report.sections.iter().flat_map(|s| &s.files).collect();
        assert!(files.iter().all(|f| f.patch.len() <= 64 * 1024));
        assert!(files.iter().map(|f| f.patch.len()).sum::<usize>() <= PATCH_LIMIT);
        assert!(files.iter().any(|f| f.truncated));
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    #[test]
    fn patch_covers_commits_edits_and_untracked_files_since_the_fork() {
        let root = std::env::temp_dir().join(format!("sigmadock-patch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@example.com"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-qm", "base"]);
        let worktree = root.join("wt");
        create(&repo, &worktree, "sigma/test", "HEAD").unwrap();
        std::fs::write(worktree.join("a.txt"), "one\ntwo\n").unwrap();
        run(&worktree, &["commit", "-qam", "agent work"]);
        std::fs::write(worktree.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(worktree.join("new.txt"), "fresh\n").unwrap();
        let (text, truncated) = patch(&worktree).unwrap();
        assert!(!truncated);
        assert!(text.contains("+two") && text.contains("+three"), "{text}");
        assert!(
            text.contains("+++ b/new.txt") && text.contains("+fresh"),
            "{text}"
        );
        assert!(clean(&repo).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
