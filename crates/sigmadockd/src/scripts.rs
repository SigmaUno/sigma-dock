//! Script approval and lifecycle orchestration. Each command has its own PTY.
use super::*;
use sigmadock_core::workspace_scripts::{Document, Phase, RunMode, ScriptStatus};

impl Daemon {
    pub(crate) fn save_scripts_worker(&mut self, worker: Worker) -> Result<Value> {
        self.store.save_worker(&worker)?;
        let value = serde_json::to_value(&worker)?;
        self.workers.insert(worker.id.clone(), worker);
        Ok(value)
    }
    fn script_document(&self, worker: &Worker) -> Result<Option<Document>> {
        Document::read(&worker.worktree)
    }
    fn approved_document(&self, worker: &Worker) -> Result<Option<Document>> {
        let document = self.script_document(worker)?;
        if let Some(document) = &document
            && !self
                .store
                .scripts_approved(&worker.project_id, &document.hash)?
        {
            bail!(
                "Repository scripts need approval. Review `sdk scripts {}` and approve its exact hash before running hooks.",
                worker.id
            );
        }
        Ok(document)
    }
    fn start_script(&mut self, worker: &Worker, name: &str, command: &str) -> Result<()> {
        let project = self.project(&worker.project_id)?;
        let env = vec![
            (
                "SIGMA_DOCK_ROOT_PATH".into(),
                project.path.to_string_lossy().into_owned(),
            ),
            (
                "SIGMA_DOCK_WORKTREE_PATH".into(),
                worker.worktree.to_string_lossy().into_owned(),
            ),
            ("SIGMA_DOCK_WORKER_ID".into(), worker.id.clone()),
            ("SIGMA_DOCK_BRANCH".into(), worker.branch.clone()),
            ("PORT".into(), worker.port.to_string()),
        ];
        let session = Arc::new(Session::spawn(
            "/bin/sh",
            &["-c".into(), command.into()],
            &env,
            &worker.worktree,
        )?);
        self.script_sessions
            .insert((worker.id.clone(), name.into()), session);
        Ok(())
    }
    pub(crate) fn begin_setup(&mut self, mut worker: Worker) -> Result<Value> {
        let document = self.script_document(&worker)?;
        worker.workspace_scripts.hash = document.as_ref().map(|doc| doc.hash.clone());
        worker.workspace_scripts.available_runs = document
            .as_ref()
            .map(|doc| doc.config.scripts.run.keys().cloned().collect())
            .unwrap_or_default();
        worker.workspace_scripts.error = None;
        worker.workspace_scripts.archive_requested = false;
        if let Some(document) = document {
            if !self
                .store
                .scripts_approved(&worker.project_id, &document.hash)?
            {
                worker.workspace_scripts.phase = Phase::AwaitingApproval;
                worker.facts.session = SessionState::NeedsInput;
                return self.save_scripts_worker(worker);
            }
            if let Some(command) = document.config.scripts.setup {
                self.start_script(&worker, "setup", &command)?;
                worker.workspace_scripts.phase = Phase::SettingUp;
                worker.facts.session = SessionState::Running;
                return self.save_scripts_worker(worker);
            }
        }
        self.start_after_setup(worker)
    }
    fn start_after_setup(&mut self, mut worker: Worker) -> Result<Value> {
        let session = self.start_session(&worker, worker.prompt.as_deref(), false)?;
        worker.workspace_scripts.phase = Phase::Ready;
        worker.workspace_scripts.error = None;
        worker.facts.session = SessionState::Running;
        worker.facts.exit_code = None;
        worker.finished_at = None;
        self.sessions.insert(worker.id.clone(), session);
        self.save_scripts_worker(worker)
    }
    pub(crate) fn scripts_call(&mut self, method: &str, params: &Value) -> Result<Value> {
        use Phase::*;
        let mut worker = self.worker(params)?.clone();
        if worker.archived {
            bail!("worker is archived");
        }
        match method {
            "workspace_scripts" => {
                let document = self.script_document(&worker)?;
                let approved = document
                    .as_ref()
                    .map(|doc| self.store.scripts_approved(&worker.project_id, &doc.hash))
                    .transpose()?
                    .unwrap_or(true);
                Ok(
                    json!({"hash":document.as_ref().map(|doc| &doc.hash), "text":document.as_ref().map(|doc| &doc.text), "config":document.as_ref().map(|doc| &doc.config), "approved":approved, "state":worker.workspace_scripts}),
                )
            }
            "approve_scripts" => {
                let document = self
                    .script_document(&worker)?
                    .context("no .sigmadock.toml to approve")?;
                if params["hash"].as_str() != Some(&document.hash) {
                    bail!("config changed since preview; review it again before approving");
                }
                self.store
                    .approve_scripts(&worker.project_id, &document.hash)?;
                if worker.workspace_scripts.phase == AwaitingApproval {
                    if worker.workspace_scripts.archive_requested {
                        self.begin_archive(worker)
                    } else {
                        self.begin_setup(worker)
                    }
                } else {
                    worker.workspace_scripts.hash = Some(document.hash);
                    worker.workspace_scripts.available_runs =
                        document.config.scripts.run.keys().cloned().collect();
                    self.save_scripts_worker(worker)
                }
            }
            "setup_worker" => {
                if worker.workspace_scripts.archive_requested {
                    bail!("archive is pending; approve scripts or retry archive");
                }
                if self
                    .script_sessions
                    .get(&(worker.id.clone(), "setup".into()))
                    .is_some_and(|s| !s.terminated())
                {
                    bail!(
                        "setup is still stopping; wait for process-group cleanup before retrying or skipping"
                    );
                }
                if !matches!(
                    worker.workspace_scripts.phase,
                    SetupFailed | AwaitingApproval
                ) {
                    bail!("setup can only be retried or skipped while blocked");
                }
                if worker.facts.session == SessionState::Lost
                    && params["acknowledge_unknown"] != true
                {
                    bail!(
                        "verify the interrupted script process has stopped, then acknowledge_unknown"
                    );
                }
                if params["skip"] == true {
                    self.start_after_setup(worker)
                } else {
                    self.begin_setup(worker)
                }
            }
            "run_script" => {
                if params["stop"] == true {
                    let name = params["name"].as_str();
                    for ((id, script), session) in &self.script_sessions {
                        if id == &worker.id
                            && script.starts_with("run:")
                            && name.is_none_or(|name| script == &format!("run:{name}"))
                        {
                            session.stop()?;
                        }
                    }
                    worker.workspace_scripts.pending_run = None;
                    return self.save_scripts_worker(worker);
                }
                if worker.workspace_scripts.phase != Ready {
                    bail!("finish or skip setup before running scripts");
                }
                let document = self
                    .approved_document(&worker)?
                    .context("no run scripts configured")?;
                let scripts = document.config.scripts;
                let name = params["name"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        scripts
                            .run
                            .iter()
                            .find(|(_, run)| run.default)
                            .map(|(name, _)| name.clone())
                    })
                    .context("specify a script name or mark one default")?;
                if !scripts.run.contains_key(&name) {
                    bail!("unknown run script {name}");
                }
                let key = (worker.id.clone(), format!("run:{name}"));
                if self
                    .script_sessions
                    .get(&key)
                    .is_some_and(|session| !session.terminated())
                {
                    bail!("run script is already running or stopping");
                }
                if !self.worker_occupies_berth(&worker) && worker.role == WorkerRole::Worker {
                    if self.project_waiting(&worker.project_id)? {
                        bail!("waiting tasks take priority over a new run session");
                    }
                    worker.berth = Some(self.free_berth(&worker.project_id, worker.berth)?);
                }
                if worker.workspace_scripts.pending_run.is_some() {
                    bail!("a run script is already waiting to start");
                }
                if scripts.run_mode == RunMode::Nonconcurrent {
                    for ((id, script), session) in &self.script_sessions {
                        if script.starts_with("run:") && id == &worker.id {
                            session.stop()?;
                        }
                    }
                }
                worker.workspace_scripts.pending_run = Some(name);
                worker.workspace_scripts.error = None;
                self.save_scripts_worker(worker)
            }
            _ => bail!("unknown scripts method"),
        }
    }
    pub(crate) fn request_archive(&mut self, params: &Value) -> Result<Value> {
        let mut worker = self.worker(params)?.clone();
        if worker.archived {
            return Ok(json!(true));
        }
        if worker.facts.session == SessionState::Lost && params["acknowledge_unknown"] != true {
            bail!(
                "verify the interrupted process stopped, then acknowledge_unknown before archiving"
            );
        }
        if self
            .sessions
            .get(&worker.id)
            .is_some_and(|session| !session.terminated() && !session.is_stopping())
        {
            bail!("stop the worker before archiving");
        }
        if worker.workspace_scripts.phase == Phase::SettingUp {
            bail!("stop setup before archiving");
        }
        if worker.workspace_scripts.phase == Phase::Archiving {
            bail!("archive hook is already running");
        }
        for ((id, _), session) in &self.script_sessions {
            if id == &worker.id {
                session.stop()?;
            }
        }
        if let Some(session) = self.sessions.get(&worker.id) {
            session.stop()?;
        }
        worker.workspace_scripts.pending_run = None;
        worker.workspace_scripts.archive_requested = true;
        worker.workspace_scripts.cleanup = params["cleanup"] == true;
        worker.workspace_scripts.force = params["force"] == true;
        self.begin_archive(worker)
    }
    fn begin_archive(&mut self, mut worker: Worker) -> Result<Value> {
        let document = self.script_document(&worker)?;
        worker.workspace_scripts.hash = document.as_ref().map(|doc| doc.hash.clone());
        worker.workspace_scripts.available_runs = document
            .as_ref()
            .map(|doc| doc.config.scripts.run.keys().cloned().collect())
            .unwrap_or_default();
        worker.workspace_scripts.error = None;
        if let Some(document) = document {
            if !self
                .store
                .scripts_approved(&worker.project_id, &document.hash)?
            {
                worker.workspace_scripts.phase = Phase::AwaitingApproval;
                worker.facts.session = SessionState::NeedsInput;
                return self.save_scripts_worker(worker);
            }
            if let Some(command) = document.config.scripts.archive {
                self.start_script(&worker, "archive", &command)?;
                worker.workspace_scripts.phase = Phase::Archiving;
                worker.facts.session = SessionState::Running;
                return self.save_scripts_worker(worker);
            }
        }
        if worker.workspace_scripts.cleanup
            && worker.worktree.exists()
            && !sigmadock_git::clean(&worker.worktree)?
        {
            bail!("worktree has uncommitted files; cleanup refused (even with --force)");
        }
        self.finish_archive(worker)
    }
    fn finish_archive(&mut self, mut worker: Worker) -> Result<Value> {
        // Child groups must be gone before deleting files, including TERM-ignoring runs.
        if self
            .sessions
            .get(&worker.id)
            .is_some_and(|s| !s.terminated())
            || self
                .script_sessions
                .iter()
                .any(|((id, _), s)| id == &worker.id && !s.terminated())
        {
            worker.workspace_scripts.phase = Phase::Archiving;
            return self.save_scripts_worker(worker);
        }
        if worker.workspace_scripts.cleanup && worker.worktree.exists() {
            let project = self.project(&worker.project_id)?;
            if !sigmadock_git::clean(&worker.worktree)? {
                bail!("worktree has uncommitted files; cleanup refused (even with --force)");
            }
            sigmadock_git::remove(&project.path, &worker.worktree)?;
        }
        self.store.clear_context(&worker.id)?;
        self.context_floor.remove(&worker.id);
        self.checkpoints.remove(&worker.id);
        self.usage.remove(&worker.id);
        worker.archived = true;
        worker.archived_at = Some(sigmadock_core::unix_time());
        worker.workspace_scripts.phase = Phase::Ready;
        worker.facts.session = SessionState::Exited;
        self.ports.release(worker.port);
        self.sessions.remove(&worker.id);
        // Retain bounded hook output until daemon restart so CLI/UI can inspect archive.
        self.forges.remove(&worker.id);
        self.save_scripts_worker(worker)?;
        Ok(json!(true))
    }
    pub(crate) fn scripts_sync(&mut self) -> Result<()> {
        let workers: Vec<_> = self
            .workers
            .values()
            .filter(|worker| !worker.archived)
            .cloned()
            .collect();
        for mut worker in workers {
            let mut changed = false;
            for ((id, name), session) in &self.script_sessions {
                if id == &worker.id
                    && let Some(name) = name.strip_prefix("run:")
                {
                    if session.facts().0 == SessionState::Exited {
                        session.stop()?;
                    }
                    let status = ScriptStatus {
                        running: !session.terminated(),
                        exit_code: session.facts().1,
                    };
                    if worker.workspace_scripts.runs.get(name) != Some(&status) {
                        worker.workspace_scripts.runs.insert(name.into(), status);
                        changed = true;
                    }
                }
            }
            let phase = worker.workspace_scripts.phase;
            if matches!(phase, Phase::SettingUp | Phase::Archiving) {
                let name = if phase == Phase::SettingUp {
                    "setup"
                } else {
                    "archive"
                };
                if let Some(session) = self.script_sessions.get(&(worker.id.clone(), name.into())) {
                    if session.facts().0 != SessionState::Exited {
                        continue;
                    }
                    session.stop()?;
                    if !session.terminated() {
                        continue;
                    }
                    let code = session.facts().1.unwrap_or(1);
                    if code != 0 && !(phase == Phase::Archiving && worker.workspace_scripts.force) {
                        worker.workspace_scripts.phase = if phase == Phase::SettingUp {
                            Phase::SetupFailed
                        } else {
                            Phase::ArchiveFailed
                        };
                        worker.workspace_scripts.error = Some(format!("{name} exited {code}"));
                        worker.facts.session = SessionState::NeedsInput;
                        worker.facts.exit_code = Some(code);
                        self.save_scripts_worker(worker)?;
                        continue;
                    }
                }
                let result = if phase == Phase::SettingUp {
                    // Approval is checked again before advancing if setup edited its config.
                    match self.script_document(&worker) {
                        Ok(doc)
                            if doc.as_ref().map(|doc| &doc.hash)
                                != worker.workspace_scripts.hash.as_ref() =>
                        {
                            self.begin_setup(worker.clone())
                        }
                        Ok(_) => self.start_after_setup(worker.clone()),
                        Err(error) => Err(error),
                    }
                } else {
                    self.finish_archive(worker.clone())
                };
                if let Err(error) = result {
                    worker.workspace_scripts.phase = if phase == Phase::SettingUp {
                        Phase::SetupFailed
                    } else {
                        Phase::ArchiveFailed
                    };
                    worker.workspace_scripts.error = Some(error.to_string());
                    worker.facts.session = SessionState::NeedsInput;
                    self.save_scripts_worker(worker)?;
                }
                continue;
            }
            if let Some(name) = worker.workspace_scripts.pending_run.clone() {
                let result = (|| -> Result<bool> {
                    let document = self
                        .approved_document(&worker)?
                        .context("run config removed")?;
                    let scripts = document.config.scripts;
                    let command = &scripts
                        .run
                        .get(&name)
                        .context("run script removed")?
                        .command;
                    if scripts.run_mode == RunMode::Nonconcurrent
                        && self.script_sessions.iter().any(|((id, name), session)| {
                            name.starts_with("run:") && !session.terminated() && id == &worker.id
                        })
                    {
                        return Ok(false);
                    }
                    self.start_script(&worker, &format!("run:{name}"), command)?;
                    worker.workspace_scripts.runs.insert(
                        name.clone(),
                        ScriptStatus {
                            running: true,
                            exit_code: None,
                        },
                    );
                    Ok(true)
                })();
                match result {
                    Ok(false) => {}
                    Ok(true) => {
                        worker.workspace_scripts.pending_run = None;
                        changed = true;
                    }
                    Err(error) => {
                        worker.workspace_scripts.pending_run = None;
                        worker.workspace_scripts.error = Some(error.to_string());
                        changed = true;
                    }
                }
            }
            if changed {
                self.save_scripts_worker(worker)?;
            }
        }
        Ok(())
    }
    pub(crate) fn worker_occupies_berth(&self, worker: &Worker) -> bool {
        !worker.archived
            && ((!worker.workspace_scripts.archive_requested
                && worker.workspace_scripts.phase.reserves_berth())
                || worker.workspace_scripts.pending_run.is_some()
                || self
                    .sessions
                    .get(&worker.id)
                    .is_some_and(|s| !s.terminated())
                || self.script_sessions.iter().any(|((id, name), s)| {
                    id == &worker.id && name.starts_with("run:") && !s.terminated()
                }))
    }
}
