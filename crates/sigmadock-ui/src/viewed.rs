//! Local review markers, independent of ephemeral terminal/diff pane state.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Viewed {
    entries: BTreeMap<String, Marker>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    blob: String,
    at: u64,
}
impl Viewed {
    fn key(worker: &str, section: &str, path: &str) -> String {
        format!("{worker}/{section}/{path}")
    }
    pub fn contains(&self, worker: &str, section: &str, path: &str, blob: &str) -> bool {
        self.entries
            .get(&Self::key(worker, section, path))
            .is_some_and(|marker| marker.blob == blob)
    }
    pub fn set(&mut self, worker: &str, section: &str, path: &str, blob: &str, viewed: bool) {
        let key = Self::key(worker, section, path);
        if viewed {
            self.entries.insert(
                key,
                Marker {
                    blob: blob.into(),
                    at: sigmadock_core::unix_time(),
                },
            );
        } else {
            self.entries.remove(&key);
        }
        while self.entries.len() > 4096 {
            if let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, marker)| marker.at)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&key);
            }
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            bail!("viewed-file state exceeds 4 MiB");
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > 4 * 1024 * 1024 {
            bail!("viewed-file state exceeds 4 MiB");
        }
        let parent = path
            .parent()
            .context("viewed-file state has no directory")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Write an atomic replacement, so a crash cannot erase the previous review markers.
        let mut file = options.open(&temporary)?;
        let result = (|| -> Result<()> {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_blobs_survive_restart_and_reset_on_change_and_worker_switch() {
        let directory = std::env::temp_dir().join(format!(
            "sigmadock-viewed-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = directory.join("viewed.json");
        let mut viewed = Viewed::default();
        viewed.set("worker1", "committed", "file.rs", "blob1", true);
        viewed.save(&path).unwrap();
        let mut loaded = Viewed::load(&path).unwrap();
        assert!(loaded.contains("worker1", "committed", "file.rs", "blob1"));
        assert!(!loaded.contains("worker1", "committed", "file.rs", "blob2"));
        assert!(!loaded.contains("worker2", "committed", "file.rs", "blob1"));
        assert!(!loaded.contains("worker1", "uncommitted", "file.rs", "blob1"));
        loaded.set("worker1", "committed", "file.rs", "blob1", false);
        loaded.save(&path).unwrap();
        assert!(
            !Viewed::load(&path)
                .unwrap()
                .contains("worker1", "committed", "file.rs", "blob1")
        );
        let _ = fs::remove_dir_all(directory);
    }
}
