//! Fork requests freeze source state before entering the normal per-project queue.
use super::*;
impl Daemon {
    pub(crate) fn fork_worker(&mut self, params: &Value) -> Result<Value> {
        let source = self.worker(params)?.clone();
        let project = self.project(&source.project_id)?;
        let mut launch = json!({"project_id":source.project_id,"title":params["title"],"prompt":params["prompt"],"agent":params["agent"].as_str().unwrap_or(&source.agent),"base":format!("refs/heads/{}", source.branch),"forge":source.forge,"usage_reporting":source.usage_reporting});
        let mut task = self.task(&launch)?;
        let waiting = self.free_berth(&task.project_id, None).is_err()
            || self.project_waiting(&task.project_id)?;
        if waiting && params["queue"] != true {
            bail!(
                "no free berth or earlier waiting tasks in this project; pass --queue to fork when a berth opens"
            );
        }
        if waiting && self.store.queued_count()? >= 1000 {
            bail!("waiting queue is full");
        }
        let include_changes = params["include_uncommitted"].as_bool().unwrap_or(false);
        if include_changes && !source.worktree.exists() {
            bail!("source worktree has been cleaned up; fork HEAD without local changes");
        }
        let snapshot_repo = if include_changes {
            &source.worktree
        } else {
            &project.path
        };
        let snapshot =
            sigmadock_git::fork_snapshot(snapshot_repo, &source.branch, include_changes, &task.id)?;
        task.base = snapshot.head.clone();
        task.forked_from = Some(source.id);
        task.fork_snapshot = Some(snapshot);
        if waiting {
            if let Err(error) = self.store.save_queued(&task) {
                let _ = sigmadock_git::release_fork_snapshot(&project.path, &task.id);
                return Err(error);
            }
            return Ok(
                json!({"queued":true,"id":task.id,"position":self.store.queued_count()?,"forked_from":task.forked_from}),
            );
        }
        launch["base"] = json!(task.base);
        launch["forked_from"] = json!(task.forked_from);
        launch["fork_snapshot"] = json!(task.fork_snapshot);
        let result = self.spawn("fork_worker", &launch, Some(&task.id), false);
        // Immediate forks aren't queued; spawn uses this preselected ID without consuming a task.
        let _ = sigmadock_git::release_fork_snapshot(&project.path, &task.id);
        result
    }
}
