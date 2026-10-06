//! Versioned SQLite state. Never stores forge tokens or terminal contents.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use sigma_dock_core::{PlanningNotes, Project, SessionState, Worker};
use std::path::Path;
pub struct Store(Connection);
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 2 {
            bail!("database is newer than this daemon");
        }
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS workers (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), data TEXT NOT NULL); COMMIT; PRAGMA foreign_keys=ON;")?;
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS planning_notes (project_id TEXT PRIMARY KEY REFERENCES projects(id), text TEXT NOT NULL, revision INTEGER NOT NULL); PRAGMA user_version=2; COMMIT;")?;
        Ok(Self(connection))
    }
    pub fn save_project(&self, project: &Project) -> Result<()> {
        self.0.execute("INSERT INTO projects(id,path,data) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![project.id, project.path.to_string_lossy(), serde_json::to_string(project)?])?;
        Ok(())
    }
    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.0.prepare("SELECT data FROM projects ORDER BY path")?;
        let values = stmt.query_map([], |row| row.get::<_, String>(0))?;
        values.map(|v| Ok(serde_json::from_str(&v?)?)).collect()
    }
    pub fn save_worker(&self, worker: &Worker) -> Result<()> {
        self.0.execute("INSERT INTO workers(id,project_id,data) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![worker.id, worker.project_id, serde_json::to_string(worker)?])?;
        Ok(())
    }
    pub fn workers(&self) -> Result<Vec<Worker>> {
        let mut stmt = self.0.prepare("SELECT data FROM workers ORDER BY rowid")?;
        let values = stmt.query_map([], |row| row.get::<_, String>(0))?;
        values.map(|v| Ok(serde_json::from_str(&v?)?)).collect()
    }
    pub fn notes(&self, project_id: &str) -> Result<PlanningNotes> {
        use rusqlite::OptionalExtension;
        let result = self
            .0
            .query_row(
                "SELECT text, revision FROM planning_notes WHERE project_id=?1",
                [project_id],
                |row| {
                    Ok(PlanningNotes {
                        project_id: project_id.into(),
                        text: row.get(0)?,
                        revision: row
                            .get::<_, i64>(1)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    })
                },
            )
            .optional()?;
        Ok(result.unwrap_or_else(|| PlanningNotes {
            project_id: project_id.into(),
            ..Default::default()
        }))
    }
    /// Compare-and-swap prevents two sessions from silently overwriting planning notes.
    pub fn write_notes(
        &mut self,
        project_id: &str,
        text: &str,
        expected_revision: u64,
    ) -> Result<PlanningNotes> {
        if text.len() > 64 * 1024 {
            bail!("planning notes exceed 64 KiB");
        }
        let tx = self.0.transaction()?;
        let current: i64 = tx.query_row(
            "SELECT COALESCE((SELECT revision FROM planning_notes WHERE project_id=?1),0)",
            [project_id],
            |row| row.get(0),
        )?;
        if u64::try_from(current)? != expected_revision {
            bail!("planning notes changed; read the current revision before writing");
        }
        let revision = current.checked_add(1).context("notes revision overflow")?;
        tx.execute("INSERT INTO planning_notes(project_id,text,revision) VALUES (?1,?2,?3) ON CONFLICT(project_id) DO UPDATE SET text=excluded.text,revision=excluded.revision", params![project_id,text,revision])?;
        tx.commit()?;
        Ok(PlanningNotes {
            project_id: project_id.into(),
            text: text.into(),
            revision: u64::try_from(revision)?,
        })
    }
    pub fn mark_disconnected(&self) -> Result<()> {
        for mut worker in self.workers()? {
            if !worker.archived
                && matches!(
                    worker.facts.session,
                    SessionState::Running | SessionState::Idle | SessionState::NeedsInput
                )
            {
                worker.facts.session = SessionState::Lost;
                self.save_worker(&worker)?;
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_workers_and_marks_lost() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = Project {
            id: "p".into(),
            path: "/repo".into(),
            name: "repo".into(),
        };
        store.save_project(&project).unwrap();
        let worker = Worker {
            id: "w".into(),
            project_id: project.id,
            title: "task".into(),
            agent: "shell".into(),
            branch: "sigma/w".into(),
            worktree: "/worker".into(),
            port: 4000,
            created_at: 0,
            archived: false,
            facts: Default::default(),
            forge: None,
            role: Default::default(),
            feedback: Default::default(),
            orchestrator_spawn: false,
        };
        store.save_worker(&worker).unwrap();
        store.mark_disconnected().unwrap();
        assert_eq!(
            store.workers().unwrap()[0].facts.session,
            SessionState::Lost
        );
    }
    #[test]
    fn planning_notes_compare_and_swap() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        store
            .save_project(&Project {
                id: "p".into(),
                path: "/p".into(),
                name: "p".into(),
            })
            .unwrap();
        assert_eq!(store.notes("p").unwrap().revision, 0);
        assert_eq!(store.write_notes("p", "first", 0).unwrap().revision, 1);
        assert!(store.write_notes("p", "overwrite", 0).is_err());
        assert_eq!(store.notes("p").unwrap().text, "first");
        assert_eq!(store.write_notes("p", "second", 1).unwrap().revision, 2);
        assert!(store.write_notes("p", &"x".repeat(65537), 2).is_err());
    }
    #[test]
    fn version_one_database_migrates_without_losing_workers() {
        let path = std::env::temp_dir().join(format!(
            "sigma-migration-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE projects(id TEXT PRIMARY KEY,path TEXT UNIQUE NOT NULL,data TEXT NOT NULL); CREATE TABLE workers(id TEXT PRIMARY KEY,project_id TEXT NOT NULL,data TEXT NOT NULL); PRAGMA user_version=1;").unwrap();
            let project = serde_json::json!({"id":"p","path":"/p","name":"p"}).to_string();
            connection
                .execute("INSERT INTO projects VALUES ('p','/p',?1)", [project])
                .unwrap();
            let old_worker = serde_json::json!({"id":"w","project_id":"p","title":"old","agent":"shell","branch":"sigma/w","worktree":"/worker","port":4200,"created_at":0,"archived":false,"facts":{"session":"running","pr":"none","checks":"unknown","review":"unknown","mergeable":null,"forge_error":null,"pr_url":null,"exit_code":null},"forge":null}).to_string();
            connection
                .execute("INSERT INTO workers VALUES ('w','p',?1)", [old_worker])
                .unwrap();
        }
        {
            let mut store = Store::open(&path).unwrap();
            let worker = store.workers().unwrap().remove(0);
            assert_eq!(worker.role, sigma_dock_core::WorkerRole::Worker);
            assert!(!worker.feedback.auto_ci);
            assert_eq!(
                store
                    .write_notes("p", "migration works", 0)
                    .unwrap()
                    .revision,
                1
            );
            let version: i32 = store
                .0
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 2);
        }
        std::fs::remove_file(path).unwrap();
    }
}
