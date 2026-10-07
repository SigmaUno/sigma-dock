//! Read-only source snapshots built with a private index. No stash, checkout or reset in the source.
use super::*;
use sigmadock_core::ForkSnapshot;
use std::fs;

const SNAPSHOT_LIMIT: u64 = 128 * 1024 * 1024;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn command(repo: &Path, index: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}
fn output(repo: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let output = command(repo, index, args).output()?;
    if !output.status.success() {
        bail!(
            "fork snapshot: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn patch(repo: &Path, args: &[&str], path: &Path) -> Result<()> {
    let file = fs::File::create(path)?;
    let output = command(repo, None, args).stdout(file).output()?;
    if !output.status.success() {
        bail!(
            "fork snapshot diff: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if fs::metadata(path)?.len() > SNAPSHOT_LIMIT {
        bail!("fork snapshot patch exceeds 128 MiB; commit large changes or fork HEAD only");
    }
    Ok(())
}
fn apply(repo: &Path, index: Option<&Path>, path: &Path, cached: bool, staged: bool) -> Result<()> {
    if fs::metadata(path)?.len() == 0 {
        return Ok(());
    }
    let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if staged {
        args.push("--index");
    }
    let output = command(repo, index, &args)
        .stdin(fs::File::open(path)?)
        .output()?;
    if !output.status.success() {
        bail!(
            "cannot apply fork snapshot: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
fn commit(repo: &Path, index: &Path, parent: &str) -> Result<String> {
    let tree = output(repo, Some(index), &["write-tree"])?;
    let result = command(
        repo,
        None,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree,
            "-p",
            parent,
            "-m",
            "SigmaDock fork snapshot",
        ],
    )
    .env("GIT_AUTHOR_NAME", "SigmaDock")
    .env("GIT_AUTHOR_EMAIL", "snapshot@sigmadock.dev")
    .env("GIT_COMMITTER_NAME", "SigmaDock")
    .env("GIT_COMMITTER_EMAIL", "snapshot@sigmadock.dev")
    .output()?;
    if !result.status.success() {
        bail!(
            "cannot create fork snapshot: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(String::from_utf8(result.stdout)?.trim().into())
}
fn scratch() -> Result<Scratch> {
    let root = std::env::temp_dir().join(format!("sigmadock-fork-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    Ok(Scratch(root))
}
fn snapshot_ref(id: &str) -> Result<String> {
    uuid::Uuid::parse_str(id).context("invalid fork task ID")?;
    Ok(format!("refs/sigmadock/forks/{id}"))
}
/// Pin the request-time commit and optional index/working-tree snapshot for durable queuing.
pub fn fork_snapshot(
    repo: &Path,
    branch: &str,
    include_changes: bool,
    id: &str,
) -> Result<ForkSnapshot> {
    let head = output(
        repo,
        None,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )?;
    let mut snapshot = ForkSnapshot {
        head: head.clone(),
        index: None,
        worktree: None,
    };
    if include_changes {
        if output(repo, None, &["symbolic-ref", "HEAD"])? != format!("refs/heads/{branch}") {
            bail!("source checkout is no longer on its worker branch");
        }
        if !output(repo, None, &["diff", "--name-only", "--diff-filter=U"])?.is_empty() {
            bail!("resolve the source's unmerged index before including local changes");
        }
        if output(repo, None, &["config", "--bool", "core.sparseCheckout"]).unwrap_or_default()
            == "true"
        {
            bail!("including local changes from sparse checkouts is not supported; fork HEAD only");
        }
        use std::os::unix::ffi::OsStrExt;
        let mut total = 0u64;
        let mut paths = command(repo, None, &["diff", "--name-only", "-z", &head, "--"])
            .output()?
            .stdout;
        paths.extend(
            command(
                repo,
                None,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            )
            .output()?
            .stdout,
        );
        for path in paths
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = repo.join(std::ffi::OsStr::from_bytes(path));
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
                    total = total.saturating_add(meta.len());
                }
                Ok(_) => bail!(
                    "cannot snapshot a changed submodule, embedded repository or special file; commit its changes or fork HEAD only"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if total > SNAPSHOT_LIMIT {
                bail!("fork local changes exceed 128 MiB; commit large changes or fork HEAD only");
            }
        }
        let scratch = scratch()?;
        let index = scratch.0.join("index");
        let staged = scratch.0.join("staged.patch");
        output(repo, Some(&index), &["read-tree", &head])?;
        patch(
            repo,
            &[
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                &head,
                "--",
            ],
            &staged,
        )?;
        apply(repo, Some(&index), &staged, true, false)?;
        let index_commit = commit(repo, &index, &head)?;
        // A private index initialized with staged additions includes even staged ignored files.
        output(repo, Some(&index), &["add", "--all", "--", "."])?;
        let working_commit = commit(repo, &index, &index_commit)?;
        if output(repo, None, &["rev-parse", "HEAD"])? != head {
            bail!("source HEAD changed during snapshot; retry the fork");
        }
        snapshot.index = Some(index_commit);
        snapshot.worktree = Some(working_commit);
    }
    output(
        repo,
        None,
        &[
            "update-ref",
            &snapshot_ref(id)?,
            snapshot.worktree.as_deref().unwrap_or(&head),
        ],
    )?;
    Ok(snapshot)
}
pub fn apply_fork_snapshot(repo: &Path, snapshot: &ForkSnapshot) -> Result<()> {
    let (Some(index), Some(worktree)) = (&snapshot.index, &snapshot.worktree) else {
        return Ok(());
    };
    let scratch = scratch()?;
    let staged = scratch.0.join("staged.patch");
    let local = scratch.0.join("local.patch");
    patch(
        repo,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            &snapshot.head,
            index,
            "--",
        ],
        &staged,
    )?;
    apply(repo, None, &staged, false, true)?;
    patch(
        repo,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            index,
            worktree,
            "--",
        ],
        &local,
    )?;
    apply(repo, None, &local, false, false)
}
pub fn release_fork_snapshot(repo: &Path, id: &str) -> Result<()> {
    output(repo, None, &["update-ref", "-d", &snapshot_ref(id)?])?;
    Ok(())
}

/// Roll back only a fresh destination whose HEAD has not advanced since creation.
pub fn rollback_unstarted_fork(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if output(path, None, &["rev-parse", "HEAD"])? != base
        || output(path, None, &["symbolic-ref", "HEAD"])? != format!("refs/heads/{branch}")
    {
        bail!("destination advanced; preserving it for inspection");
    }
    output(
        repo,
        None,
        &[
            "worktree",
            "remove",
            "--force",
            "--",
            path.to_str().context("non-UTF8 fork worktree")?,
        ],
    )?;
    output(repo, None, &["branch", "-D", "--", branch])?;
    Ok(())
}
