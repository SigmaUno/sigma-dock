//! Worktree operations deliberately refuse to discard dirty files.
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(git_output(repo, args)?.trim().into())
}
fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!("git: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into())
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
/// Inspect local refs and files only; the checks pane never fetches implicitly.
pub fn readiness(
    path: &Path,
    base_branch: Option<&str>,
    worker_branch: &str,
) -> Result<sigmadock_core::GitReadiness> {
    let head = git(path, &["rev-parse", "HEAD"])?;
    let raw = git_output(path, &["status", "--porcelain=v1", "-z"])?;
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
pub fn clean(path: &Path) -> Result<bool> {
    Ok(git(path, &["status", "--porcelain"])?.is_empty())
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
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sigmadock-base-{}-{}-{serial}",
                std::process::id(),
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
}
