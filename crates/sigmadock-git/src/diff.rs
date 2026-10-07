use super::*;
use sigmadock_core::diff::{DiffFile, DiffReport, DiffSection, DiffSectionKind};
use std::{
    io::{Read, Write},
    process::Stdio,
};

const MAX_FILES: usize = 100;
const FILE_PATCH_LIMIT: usize = 64 * 1024;
const TOTAL_PATCH_LIMIT: usize = super::PATCH_LIMIT;
const METADATA_LIMIT: usize = 16 * 1024;

/// Preserve branch refs, but freeze symbolic HEAD/expressions as a commit at launch.
pub fn recorded_base(repo: &Path, reference: &str) -> Result<String> {
    if reference.is_empty() || reference.starts_with('-') {
        bail!("invalid base ref");
    }
    let full = git(repo, &["rev-parse", "--symbolic-full-name", reference])?;
    if full.starts_with("refs/") && reference != "HEAD" {
        return Ok(full);
    }
    git(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
}

/// Read stdout without retaining an unbounded patch. Killing a capped diff never writes Git state.
fn bounded(repo: &Path, args: &[&str], limit: usize, no_index: bool) -> Result<(Vec<u8>, bool)> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run git diff")?;
    let mut stderr = child.stderr.take().context("git stderr")?;
    let diagnostic = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.by_ref().take(8192).read_to_end(&mut bytes);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .context("git stdout")?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes);
    let truncated = bytes.len() > limit;
    if truncated || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let message = diagnostic.join().unwrap_or_default();
    read?;
    if !(truncated || status.success() || no_index && status.code() == Some(1)) {
        bail!("git diff: {}", message.trim());
    }
    bytes.truncate(limit);
    Ok((bytes, truncated))
}
fn hash_bytes(repo: &Path, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().context("hash stdin")?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("cannot hash diff identity");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
fn working_blob(repo: &Path, path: &str) -> Result<String> {
    let full = repo.join(path);
    match std::fs::symlink_metadata(&full) {
        Ok(meta) if meta.file_type().is_symlink() => {
            use std::os::unix::ffi::OsStrExt;
            hash_bytes(repo, std::fs::read_link(full)?.as_os_str().as_bytes())
        }
        Ok(meta) if meta.is_file() => git(repo, &["hash-object", "--no-filters", "--", path]),
        Ok(_) => Ok("non-file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("deleted".into()),
        Err(error) => Err(error.into()),
    }
}
fn names(
    bytes: &[u8],
    truncated: bool,
    warnings: &mut Vec<String>,
) -> Vec<(String, String, Option<String>)> {
    let end = if truncated {
        bytes
            .iter()
            .rposition(|byte| *byte == 0)
            .map_or(0, |at| at + 1)
    } else {
        bytes.len()
    };
    let mut fields = bytes[..end].split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let Some(status) = fields.next().filter(|s| !s.is_empty()) {
        let Some(path) = fields.next() else {
            break;
        };
        let (old, path) = if matches!(status.first(), Some(b'R' | b'C')) {
            (Some(path), fields.next().unwrap_or_default())
        } else {
            (None, path)
        };
        if path.is_empty() {
            break;
        }
        match (
            std::str::from_utf8(status),
            std::str::from_utf8(path),
            old.map(std::str::from_utf8).transpose(),
        ) {
            (Ok(status), Ok(path), Ok(old)) => files.push((
                status[..1].to_owned(),
                path.to_owned(),
                old.map(str::to_owned),
            )),
            _ => warnings
                .push("A non-UTF8 filename was omitted; inspect the worktree in Git.".into()),
        }
    }
    if truncated {
        warnings.push("File list truncated; inspect the worktree for additional changes.".into());
    }
    files
}
pub fn diff_report(repo: &Path, base_ref: &str) -> Result<DiffReport> {
    // Resolve the selected base now and use immutable commit IDs for this snapshot.
    let base = git(
        repo,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )
    .context("diff base is unavailable; fetch/configure the base branch")?;
    let head = git(repo, &["rev-parse", "HEAD"])?;
    let merge_base = git(repo, &["merge-base", &base, &head])
        .context("worker and base have no common history")?;
    let mut report = DiffReport {
        base_ref: base_ref.into(),
        merge_base: merge_base.clone(),
        head: head.clone(),
        sections: Vec::new(),
        warnings: Vec::new(),
        truncated: false,
    };
    let mut budget = TOTAL_PATCH_LIMIT;
    let mut file_budget = MAX_FILES;
    for kind in [
        DiffSectionKind::Committed,
        DiffSectionKind::Uncommitted,
        DiffSectionKind::Untracked,
    ] {
        let mut section = DiffSection {
            kind,
            files: Vec::new(),
        };
        let (entries, counts) = if kind == DiffSectionKind::Untracked {
            let (bytes, truncated) = bounded(
                repo,
                &["ls-files", "--others", "--exclude-standard", "-z"],
                METADATA_LIMIT,
                false,
            )?;
            if truncated {
                report.truncated = true;
                report
                    .warnings
                    .push("Untracked file list truncated.".into());
            }
            let mut entries = Vec::new();
            let end = if truncated {
                bytes
                    .iter()
                    .rposition(|byte| *byte == 0)
                    .map_or(0, |at| at + 1)
            } else {
                bytes.len()
            };
            for path in bytes[..end]
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                match std::str::from_utf8(path) {
                    Ok(path) => entries.push(("A".into(), path.into(), None)),
                    Err(_) => report
                        .warnings
                        .push("A non-UTF8 untracked filename was omitted.".into()),
                }
            }
            (entries, Vec::new())
        } else {
            let mut args = vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--name-status",
                "-z",
            ];
            if kind == DiffSectionKind::Committed {
                args.extend([merge_base.as_str(), head.as_str()]);
            } else {
                args.push(head.as_str());
            }
            args.push("--");
            let (bytes, truncated) = bounded(repo, &args, METADATA_LIMIT, false)?;
            report.truncated |= truncated;
            let entries = names(&bytes, truncated, &mut report.warnings);
            args[4] = "--numstat";
            let (stats, truncated) = bounded(repo, &args, METADATA_LIMIT, false)?;
            report.truncated |= truncated;
            let counts = super::parse_numstat(&String::from_utf8_lossy(&stats));
            (entries, counts)
        };
        if entries.len() > file_budget {
            report.truncated = true;
        }
        for (status, path, old) in entries.into_iter().take(file_budget) {
            file_budget -= 1;
            let count = counts.iter().find(|file| file.path == path);
            let mut args = vec![
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
            ];
            if kind == DiffSectionKind::Untracked {
                args.extend(["--no-index", "--", "/dev/null", path.as_str()]);
            } else {
                if kind == DiffSectionKind::Committed {
                    args.extend([merge_base.as_str(), head.as_str()]);
                } else {
                    args.push(head.as_str());
                }
                args.push("--");
                if let Some(old) = &old {
                    args.push(old);
                }
                args.push(&path);
            }
            let before = if kind == DiffSectionKind::Committed {
                None
            } else {
                Some(working_blob(repo, &path)?)
            };
            let (mut bytes, mut truncated) = bounded(
                repo,
                &args,
                FILE_PATCH_LIMIT.min(budget),
                kind == DiffSectionKind::Untracked,
            )?;
            budget = budget.saturating_sub(bytes.len());
            if truncated {
                let end = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(0, |end| end + 1);
                bytes.truncate(end);
                report.truncated = true;
            }
            let patch = String::from_utf8_lossy(&bytes).into_owned();
            let old_ref = if kind == DiffSectionKind::Committed {
                merge_base.as_str()
            } else {
                head.as_str()
            };
            let old_blob = git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{old_ref}:{}", old.as_deref().unwrap_or(&path)),
                ],
            )
            .unwrap_or_else(|_| "absent".into());
            let blob = if kind == DiffSectionKind::Committed {
                git(repo, &["rev-parse", "--verify", &format!("{head}:{path}")])
                    .unwrap_or_else(|_| "deleted".into())
            } else {
                working_blob(repo, &path)?
            };
            if before.as_ref().is_some_and(|before| before != &blob) {
                truncated = true;
                report.truncated = true;
                report.warnings.push(format!(
                    "{path} changed during refresh; refresh again before marking it viewed."
                ));
            }
            let blob_id = hash_bytes(
                repo,
                format!("{old_blob}\n{blob}\n{status}\n{patch}").as_bytes(),
            )?;
            let binary = count.is_some_and(|c| c.added.is_none())
                || patch.contains("Binary files ")
                || patch.contains("GIT binary patch");
            let (added, removed) = if let Some(count) = count {
                (count.added, count.removed)
            } else if binary {
                (None, None)
            } else {
                (
                    Some(
                        patch
                            .lines()
                            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                            .count() as u64,
                    ),
                    Some(0),
                )
            };
            section.files.push(DiffFile {
                path,
                status,
                added,
                removed,
                binary,
                blob_id,
                patch,
                truncated,
            });
        }
        report.sections.push(section);
    }
    if report.truncated {
        report.warnings.push("Diff limited to 100 file entries, 64 KiB per file and 256 KiB overall; open the worktree for the rest.".into());
    }
    Ok(report)
}
