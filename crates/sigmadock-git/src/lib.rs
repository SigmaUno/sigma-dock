//! Worktree operations deliberately refuse to discard dirty files.
use anyhow::{Context, Result, bail};
use sigmadock_core::summary::{Changes, FileChange, MAX_COMMITS};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!("git: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
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
pub fn diff(path: &Path) -> Result<String> {
    git(path, &["diff", "--stat", "HEAD"])
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
}
