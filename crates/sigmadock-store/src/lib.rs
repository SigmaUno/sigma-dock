//! Versioned SQLite state. Stores bounded local transcript checkpoints; never stores forge tokens.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use sigmadock_core::{
    PlanningNotes, Project, QueuedTask, SessionContext, SessionState, Worker, unix_time,
};
use std::path::Path;
pub struct Store(Connection);
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 4 {
            bail!("database is newer than this daemon");
        }
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS workers (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), data TEXT NOT NULL); COMMIT; PRAGMA foreign_keys=ON;")?;
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS planning_notes (project_id TEXT PRIMARY KEY REFERENCES projects(id), text TEXT NOT NULL, revision INTEGER NOT NULL); PRAGMA user_version=2; COMMIT;")?;
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS session_context (worker_id TEXT PRIMARY KEY REFERENCES workers(id), recorded_at INTEGER NOT NULL, data TEXT NOT NULL); PRAGMA user_version=3; COMMIT;")?;
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS task_queue (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id), data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS hidden_projects (id TEXT PRIMARY KEY REFERENCES projects(id)); PRAGMA user_version=4; COMMIT;")?;
        let store = Self(connection);
        store.prune_context()?;
        Ok(store)
    }
    pub fn queue(&self) -> Result<Vec<QueuedTask>> {
        let mut stmt = self
            .0
            .prepare("SELECT data FROM task_queue ORDER BY sequence")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn next_queued(&self) -> Result<Option<QueuedTask>> {
        use rusqlite::OptionalExtension;
        let data: Option<String> = self
            .0
            .query_row(
                "SELECT data FROM task_queue ORDER BY sequence LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }
    pub fn queued_count(&self) -> Result<usize> {
        let count: i64 = self
            .0
            .query_row("SELECT count(*) FROM task_queue", [], |row| row.get(0))?;
        Ok(usize::try_from(count)?)
    }
    pub fn save_queued(&self, task: &QueuedTask) -> Result<()> {
        self.0.execute("INSERT INTO task_queue(id,project_id,data) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![task.id, task.project_id, serde_json::to_string(task)?])?;
        Ok(())
    }
    pub fn cancel_queued(&self, id: &str) -> Result<()> {
        if self.0.execute("DELETE FROM task_queue WHERE id=?1", [id])? != 1 {
            bail!("queued task not found");
        }
        Ok(())
    }
    /// Publish the worker and consume the task atomically after a successful launch.
    pub fn finish_queued(&mut self, worker: &Worker) -> Result<()> {
        let tx = self.0.transaction()?;
        tx.execute(
            "INSERT INTO workers(id,project_id,data) VALUES (?1,?2,?3)",
            params![worker.id, worker.project_id, serde_json::to_string(worker)?],
        )?;
        if tx.execute("DELETE FROM task_queue WHERE id=?1", [&worker.id])? != 1 {
            bail!("queued task not found");
        }
        tx.commit()?;
        Ok(())
    }
    pub fn recover_queue(&self) -> Result<()> {
        for mut task in self.queue()? {
            if task.starting {
                task.starting = false;
                task.last_error = Some("Task startup was interrupted. Inspect its sigma/ID branch, worktree and any surviving process before retrying or cancelling.".into());
                self.save_queued(&task)?;
            }
        }
        Ok(())
    }
    pub fn max_workers(&self) -> Result<Option<usize>> {
        use rusqlite::OptionalExtension;
        let value: Option<i64> = self
            .0
            .query_row(
                "SELECT value FROM settings WHERE key='max_workers'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value.map(|value| Ok(usize::try_from(value)?)).transpose()
    }
    pub fn set_max_workers(&self, value: usize) -> Result<()> {
        self.0.execute("INSERT INTO settings(key,value) VALUES ('max_workers',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [i64::try_from(value)?])?;
        Ok(())
    }
    /// Hide the project while retaining archived history and its repository.
    pub fn remove_project(&self, id: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO hidden_projects(id) VALUES (?1) ON CONFLICT DO NOTHING",
            [id],
        )?;
        Ok(())
    }
    pub fn save_project(&self, project: &Project) -> Result<()> {
        self.0.execute("INSERT INTO projects(id,path,data) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![project.id, project.path.to_string_lossy(), serde_json::to_string(project)?])?;
        self.0
            .execute("DELETE FROM hidden_projects WHERE id=?1", [&project.id])?;
        Ok(())
    }
    pub fn project_at(&self, path: &Path) -> Result<Option<Project>> {
        use rusqlite::OptionalExtension;
        let data: Option<String> = self
            .0
            .query_row(
                "SELECT data FROM projects WHERE path=?1",
                [path.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }
    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.0.prepare("SELECT data FROM projects WHERE id NOT IN (SELECT id FROM hidden_projects) ORDER BY path")?;
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
    pub fn save_context(&self, context: &SessionContext) -> Result<()> {
        if context.text.len() > 16 * 1024 {
            bail!("session context exceeds 16 KiB");
        }
        self.0.execute("INSERT INTO session_context(worker_id,recorded_at,data) VALUES (?1,?2,?3) ON CONFLICT(worker_id) DO UPDATE SET recorded_at=excluded.recorded_at,data=excluded.data", params![context.worker_id,i64::try_from(context.recorded_at)?,serde_json::to_string(context)?])?;
        self.prune_context()
    }
    pub fn context(&self, worker_id: &str) -> Result<Option<SessionContext>> {
        self.prune_context()?;
        use rusqlite::OptionalExtension;
        let data: Option<String> = self
            .0
            .query_row(
                "SELECT data FROM session_context WHERE worker_id=?1",
                [worker_id],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }
    pub fn clear_context(&self, worker_id: &str) -> Result<()> {
        self.0.execute(
            "DELETE FROM session_context WHERE worker_id=?1",
            [worker_id],
        )?;
        Ok(())
    }
    pub fn prune_context(&self) -> Result<()> {
        self.0.execute(
            "DELETE FROM session_context WHERE recorded_at < ?1",
            [i64::try_from(unix_time().saturating_sub(7 * 86400))?],
        )?;
        self.0.execute("DELETE FROM session_context WHERE worker_id IN (SELECT worker_id FROM session_context ORDER BY recorded_at DESC, rowid DESC LIMIT -1 OFFSET 512)",[])?;
        Ok(())
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
            base_branch: None,
            id: "p".into(),
            path: "/repo".into(),
            name: "repo".into(),
        };
        store.save_project(&project).unwrap();
        let worker = Worker {
            base_warning: None,
            berth: None,
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
            archived_at: None,
            prompt: None,
            finished_at: None,
            usage_reporting: false,
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
                base_branch: None,
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
            assert_eq!(worker.berth, None);
            assert_eq!(worker.role, sigmadock_core::WorkerRole::Worker);
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
            assert_eq!(version, 4);
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    fn fixture() -> (Store, Worker) {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .save_project(&Project {
                base_branch: None,
                id: "p".into(),
                path: "/p".into(),
                name: "p".into(),
            })
            .unwrap();
        let worker: Worker = serde_json::from_value(serde_json::json!({"id":"w","project_id":"p","title":"task","agent":"shell","branch":"sigma/w","worktree":"/w","port":4200,"created_at":0,"archived":false,"facts":{"session":"running","pr":"none","checks":"unknown","review":"unknown","mergeable":null,"forge_error":null,"pr_url":null,"exit_code":null},"forge":null})).unwrap();
        store.save_worker(&worker).unwrap();
        (store, worker)
    }
    #[test]
    fn checkpoint_preserves_last_state_without_claiming_process_survival() {
        let (store, worker) = fixture();
        store
            .save_context(&SessionContext {
                worker_id: worker.id.clone(),
                recorded_at: unix_time(),
                last_activity: unix_time(),
                state: SessionState::Running,
                pid: Some(123),
                text: "where it stopped".into(),
                truncated: false,
            })
            .unwrap();
        store.mark_disconnected().unwrap();
        assert_eq!(
            store.workers().unwrap()[0].facts.session,
            SessionState::Lost
        );
        assert_eq!(
            store.context(&worker.id).unwrap().unwrap().state,
            SessionState::Running
        );
        store.clear_context(&worker.id).unwrap();
        assert!(store.context(&worker.id).unwrap().is_none());
    }
    #[test]
    fn retention_and_size_are_bounded() {
        let (store, worker) = fixture();
        let mut context = SessionContext {
            worker_id: worker.id,
            recorded_at: unix_time() - 8 * 86400,
            last_activity: 0,
            state: SessionState::Exited,
            pid: None,
            text: "old".into(),
            truncated: false,
        };
        store.save_context(&context).unwrap();
        assert!(store.context(&context.worker_id).unwrap().is_none());
        context.recorded_at = unix_time();
        context.text = "x".repeat(16385);
        assert!(store.save_context(&context).is_err());
        for index in 0..520 {
            store
                .0
                .execute(
                    "INSERT INTO workers(id,project_id,data) VALUES (?1,'p','{}')",
                    [format!("worker-{index}")],
                )
                .unwrap();
            context.worker_id = format!("worker-{index}");
            context.text = "tail".into();
            store.save_context(&context).unwrap();
        }
        let count: i64 = store
            .0
            .query_row("SELECT count(*) FROM session_context", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 512);
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    fn task(id: &str, project_id: &str) -> QueuedTask {
        QueuedTask {
            fetch_base: false,
            id: id.into(),
            project_id: project_id.into(),
            title: id.into(),
            agent: "shell".into(),
            prompt: None,
            base: "main".into(),
            forge: None,
            usage_reporting: false,
            created_at: 1,
            last_error: None,
            starting: false,
        }
    }
    fn project(id: &str) -> Project {
        Project {
            base_branch: None,
            id: id.into(),
            path: format!("/{id}").into(),
            name: id.into(),
        }
    }
    #[test]
    fn version_three_queue_survives_restart_and_updates_preserve_fifo() {
        let path = std::env::temp_dir().join(format!(
            "sigmadock-queue-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let store = Store::open(&path).unwrap();
            store.save_project(&project("p")).unwrap();
            store.save_project(&project("other")).unwrap();
            store.0.execute_batch("DROP TABLE task_queue; DROP TABLE settings; DROP TABLE hidden_projects; PRAGMA user_version=3;").unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let version: i32 = store
                .0
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 4);
            store.save_queued(&task("first", "p")).unwrap();
            store.save_queued(&task("second", "other")).unwrap();
            let mut first = task("first", "p");
            first.starting = true;
            store.save_queued(&first).unwrap();
            store.set_max_workers(3).unwrap();
        }
        {
            let mut store = Store::open(&path).unwrap();
            store.recover_queue().unwrap();
            let tasks = store.queue().unwrap();
            assert_eq!(
                tasks
                    .iter()
                    .map(|task| task.id.as_str())
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
            assert!(!tasks[0].starting);
            assert!(
                tasks[0]
                    .last_error
                    .as_ref()
                    .unwrap()
                    .contains("interrupted")
            );
            assert_eq!(store.max_workers().unwrap(), Some(3));
            // An update does not send the failed task to the back of the queue.
            store.save_queued(&task("first", "p")).unwrap();
            let worker: Worker = serde_json::from_value(serde_json::json!({"id":"first","project_id":"p","title":"first","agent":"shell","branch":"sigma/first","worktree":"/w","port":4200,"created_at":0,"archived":false,"facts":{"session":"running","pr":"none","checks":"unknown","review":"unknown","mergeable":null,"forge_error":null,"pr_url":null,"exit_code":null},"forge":null,"berth":2})).unwrap();
            store.finish_queued(&worker).unwrap();
            assert_eq!(store.workers().unwrap()[0].berth, Some(2));
            assert_eq!(store.queue().unwrap()[0].id, "second");
            // Failure rolls back worker creation as well as task consumption.
            let mut missing = worker.clone();
            missing.id = "missing".into();
            assert!(store.finish_queued(&missing).is_err());
            assert_eq!(store.workers().unwrap().len(), 1);
            store.cancel_queued("second").unwrap();
            assert!(store.queue().unwrap().is_empty());
            assert!(store.cancel_queued("second").is_err());
            store.remove_project("p").unwrap();
            assert!(
                !store
                    .projects()
                    .unwrap()
                    .iter()
                    .any(|project| project.id == "p")
            );
            assert_eq!(store.workers().unwrap().len(), 1);
            store.save_project(&project("p")).unwrap();
            assert_eq!(store.projects().unwrap().len(), 2);
        }
        std::fs::remove_file(path).unwrap();
    }
}
