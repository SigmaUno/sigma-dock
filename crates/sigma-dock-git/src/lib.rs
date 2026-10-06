//! Worktree operations deliberately refuse to discard dirty files.
use anyhow::{Context, Result, bail};
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
pub fn rollback(repo: &Path, path: &Path, branch: &str) {
    if remove(repo, path).is_ok() {
        let _ = git(repo, &["branch", "-d", branch]);
    }
}
