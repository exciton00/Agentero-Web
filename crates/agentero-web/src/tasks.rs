//! Server-side task center: in-memory job registry + tokio runners.
//!
//! The renderer speaks the desktop `features::jobs` contract unchanged
//! (docs/development/webui.md §2.4), so the wire shapes here mirror
//! `src-tauri/src/features/jobs/mod.rs`:
//!
//! * `job:changed` → `{ "job": JobSnapshot }` (queued → running → terminal);
//! * `job:completed` / `job:failed` → `JobTerminalPayload`;
//! * `job:progress` is emitted by the domain code itself (byte/count payload
//!   `AssetDownloadProgress`, forwarded by `WebHostHooks`).
//!
//! Jobs run on the server (`spawn` + `tokio::spawn`); renderer execution
//! (`job:offer`) is not used in v1 — layout analysis is deferred, so every
//! snapshot reports `host: "host"`. Cancellation is cooperative: `job_cancel`
//! fires the job's [`CancellationToken`], flips the snapshot to `cancelled`
//! immediately (like the desktop JobCenter), and installs a process-wide probe
//! so `agentero-core` pollers (`check_task_not_cancelled`) can stop early.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;

pub const JOB_CHANGED_EVENT: &str = "job:changed";
pub const JOB_COMPLETED_EVENT: &str = "job:completed";
pub const JOB_FAILED_EVENT: &str = "job:failed";

/// Job kinds shared with the desktop Host (`JobKind` in bindings.ts).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    ParseRefs,
    ParseBody,
    LayoutAnalyze,
    LayoutTranslate,
    DownloadAssets,
    PageCount,
    WikiReindex,
    RecognizeMetadata,
    Import,
    ConnectorSync,
    ModelDownload,
    CitingScan,
    LibraryIo,
    MetadataRefresh,
    LatexCompile,
    JevSmartHighlights,
}

/// Queue lane (`JobLane`); the server currently runs everything in order.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum JobLane {
    Focus,
    #[default]
    Normal,
    Idle,
}

/// Job lifecycle (`JobState` in bindings.ts).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled | JobState::Skipped
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DepPolicy {
    AllSettled,
    #[default]
    AllSucceeded,
}

/// Execution host of a job (`ExecHost`). The headless host always executes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ExecHost {
    #[default]
    Host,
    Renderer,
}

/// One job row, wire-compatible with the desktop `JobSnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub id: String,
    pub kind: JobKind,
    pub lane: JobLane,
    pub state: JobState,
    /// Vault root as passed by the enqueuing caller, verbatim.
    pub vault_id: String,
    /// Canonical vault root: filesystem operations + dedupe identity.
    pub vault_path: String,
    pub paper_path: Option<String>,
    pub fingerprint: String,
    pub depends_on: Vec<String>,
    pub dep_policy: DepPolicy,
    pub progress: Option<f32>,
    pub phase: Option<String>,
    pub error: Option<String>,
    pub force: bool,
    pub params: Option<Value>,
    pub host: ExecHost,
}

/// `job:changed` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobChangedPayload {
    pub job: JobSnapshot,
}

/// `job:completed` / `job:failed` payload (`JobTerminalPayload`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobTerminalPayload {
    pub job_id: String,
    pub kind: JobKind,
    pub vault_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_id: Option<String>,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Enqueue parameters for a server-side task.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: JobKind,
    /// Vault root exactly as the caller passed it (`JobSnapshot.vault_id`).
    pub vault_id: String,
    /// Canonical vault root (filesystem access + dedupe identity).
    pub vault_path: String,
    pub paper_path: Option<String>,
    pub lane: JobLane,
    pub force: bool,
    pub params: Option<Value>,
}

impl JobSpec {
    /// Resolve the vault and build a spec for `kind`.
    pub fn new(kind: JobKind, vault_path: &str) -> Result<Self, String> {
        let trimmed = vault_path.trim();
        if trimmed.is_empty() {
            return Err("vaultPath is required".to_string());
        }
        let root = agentero_core::fs::resolve_vault(trimmed).map_err(|error| error.to_string())?;
        let canonical = agentero_core::fs::canonicalize_best_effort(&root);
        Ok(Self {
            kind,
            vault_id: trimmed.to_string(),
            vault_path: canonical.to_string_lossy().to_string(),
            paper_path: None,
            lane: JobLane::Normal,
            force: false,
            params: None,
        })
    }

    pub fn paper_path(mut self, paper_path: impl Into<String>) -> Self {
        let path = paper_path.into().replace('\\', "/");
        let path = path.trim_matches('/').to_string();
        self.paper_path = (!path.is_empty()).then_some(path);
        self
    }

    pub fn lane(mut self, lane: Option<JobLane>) -> Self {
        self.lane = lane.unwrap_or_default();
        self
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn params(mut self, params: Option<Value>) -> Self {
        self.params = params;
        self
    }

    fn fingerprint(&self) -> String {
        let params = match &self.params {
            Some(value) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                value.to_string().hash(&mut hasher);
                format!(":{:016x}", hasher.finish())
            }
            None => String::new(),
        };
        format!(
            "{:?}:v1:force:{}{params}:{}",
            self.kind,
            self.force,
            self.paper_path.as_deref().unwrap_or_default()
        )
    }

    fn snapshot(&self, id: String) -> JobSnapshot {
        JobSnapshot {
            id,
            kind: self.kind,
            lane: self.lane,
            state: JobState::Queued,
            vault_id: self.vault_id.clone(),
            vault_path: self.vault_path.clone(),
            paper_path: self.paper_path.clone(),
            fingerprint: self.fingerprint(),
            depends_on: Vec::new(),
            dep_policy: DepPolicy::AllSucceeded,
            progress: Some(0.0),
            phase: Some("queued".to_string()),
            error: None,
            force: self.force,
            params: self.params.clone(),
            host: ExecHost::Host,
        }
    }
}

/// Handle given to a running task: progress reporting + cooperative cancel.
#[derive(Clone)]
pub struct JobHandle {
    id: String,
    cancel: CancellationToken,
    center: TaskCenter,
}

impl JobHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// True once `job_cancel` fired for this job (or core cancelled the task id).
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled() || agentero_core::cancel::is_cancelled(&self.id)
    }

    /// Report coarse progress (0–100) / a phase label; emits `job:changed`.
    pub fn set_progress(&self, progress: Option<f32>, phase: Option<String>) {
        self.center.update_progress(&self.id, progress, phase);
    }

    /// Bail out early when the job was cancelled.
    pub fn ensure_running(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("job cancelled".to_string())
        } else {
            Ok(())
        }
    }
}

enum JobOutcome {
    Succeeded,
    Failed(String),
    Cancelled,
}

struct Job {
    snapshot: JobSnapshot,
    cancel: CancellationToken,
    /// Dedupe identity while queued/running (released on any terminal state).
    active_key: String,
}

#[derive(Default)]
struct State {
    jobs: HashMap<String, Job>,
    /// Insertion order, so `job_list` is stable for the task panel.
    order: Vec<String>,
    /// Dedupe fingerprint → active job id.
    active: HashMap<String, String>,
    /// Task ids cancelled via `job_cancel`, polled by core through the probe.
    cancelled: HashSet<String>,
}

struct Inner {
    state: Mutex<State>,
    bus: EventBus,
    seq: AtomicU64,
}

/// In-memory task registry; cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct TaskCenter {
    inner: Arc<Inner>,
}

impl TaskCenter {
    pub fn new(bus: EventBus) -> Self {
        let center = Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                bus,
                seq: AtomicU64::new(1),
            }),
        };
        // Core pollers (`check_task_not_cancelled`) consult this probe; without
        // it a cancelled long task would keep running to completion.
        let inner = center.inner.clone();
        agentero_core::cancel::install_cancel_probe(move |task_id| {
            inner
                .state
                .lock()
                .map(|state| state.cancelled.contains(task_id))
                .unwrap_or(false)
        });
        center
    }

    /// Register a job, emit `job:changed(queued)` and spawn its runner.
    ///
    /// The runner is a boxed future: it owns the actual domain call and returns
    /// `Ok(())` on success / `Err(message)` on failure. Cancellation is also
    /// raced at the future level, so jobs that do not poll core's cancel probe
    /// still settle promptly.
    pub fn spawn<F>(&self, spec: JobSpec, run: F) -> JobSnapshot
    where
        F: FnOnce(
                JobHandle,
            )
                -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            + Send
            + 'static,
    {
        let id = self.next_id();
        let snapshot = spec.snapshot(id.clone());
        let cancel = CancellationToken::new();
        {
            let mut state = self.lock();
            // Dedupe against an identical active job, like the desktop center.
            if let Some(existing) = state.active.get(&snapshot.fingerprint).and_then(|id| {
                state
                    .jobs
                    .get(id)
                    .map(|job| (id.clone(), job.snapshot.clone()))
            }) {
                if !existing.1.state.is_terminal() {
                    return existing.1;
                }
            }
            state
                .active
                .insert(snapshot.fingerprint.clone(), snapshot.id.clone());
            state.jobs.insert(
                snapshot.id.clone(),
                Job {
                    snapshot: snapshot.clone(),
                    cancel: cancel.clone(),
                    active_key: snapshot.fingerprint.clone(),
                },
            );
            state.order.push(snapshot.id.clone());
        }
        self.emit_changed(&snapshot);

        let center = self.clone();
        let job_id = snapshot.id.clone();
        tokio::spawn(async move {
            center.run_job(job_id, cancel, run).await;
        });
        snapshot
    }

    /// Every job, newest registration order preserved. Filters mirror the
    /// desktop `job_list`: canonical vault root and exact paper path.
    pub fn list(&self, vault: Option<&str>, path: Option<&str>) -> Vec<JobSnapshot> {
        let vault = vault
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                agentero_core::fs::canonicalize_best_effort(std::path::Path::new(value))
                    .to_string_lossy()
                    .to_string()
            });
        let path = path
            .map(|value| value.replace('\\', "/"))
            .map(|value| value.trim_matches('/').to_string());
        let state = self.lock();
        state
            .order
            .iter()
            .filter_map(|id| state.jobs.get(id))
            .filter(|job| {
                vault
                    .as_deref()
                    .is_none_or(|vault| job.snapshot.vault_path == vault)
                    && path
                        .as_deref()
                        .is_none_or(|path| job.snapshot.paper_path.as_deref() == Some(path))
            })
            .map(|job| job.snapshot.clone())
            .collect()
    }

    pub fn snapshot(&self, job_id: &str) -> Option<JobSnapshot> {
        self.lock().jobs.get(job_id).map(|job| job.snapshot.clone())
    }

    /// Cancel a queued/running job. Returns `false` when it is unknown or
    /// already terminal (desktop `job_cancel` contract).
    pub fn cancel(&self, job_id: &str) -> bool {
        let snapshot = {
            let mut guard = self.lock();
            let (snapshot, active_key) = {
                let Some(job) = guard.jobs.get_mut(job_id) else {
                    return false;
                };
                if job.snapshot.state.is_terminal() {
                    return false;
                }
                job.cancel.cancel();
                job.snapshot.state = JobState::Cancelled;
                job.snapshot.progress = None;
                job.snapshot.phase = Some("cancelled".to_string());
                (job.snapshot.clone(), job.active_key.clone())
            };
            if guard.active.get(&active_key) == Some(&snapshot.id) {
                guard.active.remove(&active_key);
            }
            guard.cancelled.insert(job_id.to_string());
            snapshot
        };
        self.emit_changed(&snapshot);
        true
    }

    /// Apply a progress/terminal report (renderer-executor protocol; a no-op
    /// path for server-run jobs, kept for wire compatibility).
    pub fn report(
        &self,
        job_id: &str,
        progress: Option<f32>,
        phase: Option<String>,
        error: Option<String>,
        state: Option<JobState>,
    ) -> Option<JobSnapshot> {
        let snapshot = {
            let mut guard = self.lock();
            let (snapshot, active_key, terminal) = {
                let job = guard.jobs.get_mut(job_id)?;
                if job.snapshot.state != JobState::Running {
                    return None;
                }
                if let Some(progress) = progress {
                    job.snapshot.progress = Some(progress.clamp(0.0, 100.0));
                }
                if let Some(phase) = phase {
                    job.snapshot.phase = Some(phase);
                }
                if let Some(error) = error {
                    job.snapshot.error = Some(error);
                }
                if let Some(next) = state {
                    job.snapshot.state = next;
                }
                (
                    job.snapshot.clone(),
                    job.active_key.clone(),
                    job.snapshot.state.is_terminal(),
                )
            };
            if terminal {
                if guard.active.get(&active_key) == Some(&snapshot.id) {
                    guard.active.remove(&active_key);
                }
                guard.cancelled.remove(&snapshot.id);
            }
            snapshot
        };
        // This snapshot is emitted without the terminal-only lifecycle event:
        // server-run jobs go through `finish`; renderer reports are never
        // terminal in v1.
        self.emit_changed(&snapshot);
        Some(snapshot)
    }

    async fn run_job<F>(self, id: String, cancel: CancellationToken, run: F)
    where
        F: FnOnce(
            JobHandle,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>,
    {
        if cancel.is_cancelled() {
            self.finish(&id, JobOutcome::Cancelled);
            return;
        }
        self.mark_running(&id);
        let handle = JobHandle {
            id: id.clone(),
            cancel: cancel.clone(),
            center: self.clone(),
        };
        let future = run(handle);
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => JobOutcome::Cancelled,
            result = future => match result {
                Ok(()) if !cancel.is_cancelled() => JobOutcome::Succeeded,
                Ok(()) => JobOutcome::Cancelled,
                Err(error) if cancel.is_cancelled() => {
                    log::debug!("job {id} error after cancel: {error}");
                    JobOutcome::Cancelled
                }
                Err(error) => JobOutcome::Failed(error),
            },
        };
        self.finish(&id, outcome);
    }

    fn next_id(&self) -> String {
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        format!("job-{}-{seq}", now_ms())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn mark_running(&self, id: &str) {
        let snapshot = {
            let mut state = self.lock();
            let Some(job) = state.jobs.get_mut(id) else {
                return;
            };
            if job.snapshot.state != JobState::Queued {
                return;
            }
            job.snapshot.state = JobState::Running;
            job.snapshot.phase = Some("running".to_string());
            job.snapshot.progress.get_or_insert(0.0);
            job.snapshot.clone()
        };
        self.emit_changed(&snapshot);
    }

    fn update_progress(&self, id: &str, progress: Option<f32>, phase: Option<String>) {
        let snapshot = {
            let mut state = self.lock();
            let Some(job) = state.jobs.get_mut(id) else {
                return;
            };
            if job.snapshot.state.is_terminal() {
                return;
            }
            if let Some(progress) = progress {
                job.snapshot.progress = Some(progress.clamp(0.0, 100.0));
            }
            if let Some(phase) = phase {
                job.snapshot.phase = Some(phase);
            }
            job.snapshot.clone()
        };
        self.emit_changed(&snapshot);
    }

    fn finish(&self, id: &str, outcome: JobOutcome) {
        let snapshot = {
            let mut guard = self.lock();
            let (snapshot, active_key, already_terminal) = {
                let Some(job) = guard.jobs.get_mut(id) else {
                    return;
                };
                if job.snapshot.state.is_terminal() {
                    (job.snapshot.clone(), String::new(), true)
                } else {
                    match &outcome {
                        JobOutcome::Succeeded => {
                            job.snapshot.state = JobState::Succeeded;
                            job.snapshot.progress = Some(100.0);
                            job.snapshot.phase = Some("completed".to_string());
                            job.snapshot.error = None;
                        }
                        JobOutcome::Failed(error) => {
                            job.snapshot.state = JobState::Failed;
                            job.snapshot.phase = Some("failed".to_string());
                            job.snapshot.error = Some(error.clone());
                        }
                        JobOutcome::Cancelled => {
                            job.snapshot.state = JobState::Cancelled;
                            job.snapshot.progress = None;
                            job.snapshot.phase = Some("cancelled".to_string());
                        }
                    }
                    (job.snapshot.clone(), job.active_key.clone(), false)
                }
            };
            if already_terminal {
                // `cancel` already settled it; just release the cancel probe.
                guard.cancelled.remove(id);
                return;
            }
            if guard.active.get(&active_key) == Some(&snapshot.id) {
                guard.active.remove(&active_key);
            }
            guard.cancelled.remove(id);
            snapshot
        };
        self.emit_changed(&snapshot);
        match outcome {
            JobOutcome::Succeeded => self.emit_terminal(&snapshot, JOB_COMPLETED_EVENT, None),
            JobOutcome::Failed(error) => {
                self.emit_terminal(&snapshot, JOB_FAILED_EVENT, Some(error))
            }
            // Cancellation is reported through `job:changed` only, matching the
            // desktop lifecycle contract (`emit_job_terminal` skips cancelled).
            JobOutcome::Cancelled => {}
        }
    }

    fn emit_changed(&self, snapshot: &JobSnapshot) {
        self.inner.bus.emit(
            JOB_CHANGED_EVENT,
            &JobChangedPayload {
                job: snapshot.clone(),
            },
        );
    }

    fn emit_terminal(&self, snapshot: &JobSnapshot, event: &str, error: Option<String>) {
        let paper_id = snapshot
            .paper_path
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next())
            .filter(|segment| !segment.is_empty())
            .map(str::to_string);
        self.inner.bus.emit(
            event,
            &JobTerminalPayload {
                job_id: snapshot.id.clone(),
                kind: snapshot.kind,
                vault_id: snapshot.vault_id.clone(),
                paper_id,
                timestamp: now_ms(),
                error,
            },
        );
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn center() -> (
        TaskCenter,
        tokio::sync::broadcast::Receiver<crate::events::EventFrame>,
    ) {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        (TaskCenter::new(bus), rx)
    }

    fn drain(
        rx: &mut tokio::sync::broadcast::Receiver<crate::events::EventFrame>,
    ) -> Vec<crate::events::EventFrame> {
        let mut frames = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            frames.push(frame);
        }
        frames
    }

    fn vault() -> String {
        let dir = std::env::temp_dir().join(format!("agentero-web-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp vault");
        dir.to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn spawn_emits_changed_and_completes() {
        let (center, mut rx) = center();
        let spec = JobSpec::new(JobKind::Import, &vault()).expect("spec");
        let snapshot = center.spawn(spec, |_handle| Box::pin(async { Ok(()) }));
        assert_eq!(snapshot.state, JobState::Queued);

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frames = drain(&mut rx);
                if frames
                    .iter()
                    .any(|frame| frame.event == JOB_COMPLETED_EVENT)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job completed");

        let jobs = center.list(None, None);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Succeeded);
        assert_eq!(jobs[0].progress, Some(100.0));
    }

    #[tokio::test]
    async fn cancel_settles_running_job() {
        let (center, mut rx) = center();
        let spec = JobSpec::new(JobKind::DownloadAssets, &vault()).expect("spec");
        let snapshot = center.spawn(spec, |_handle| {
            Box::pin(async {
                // A runner that ignores cancellation entirely still settles
                // promptly because the future is raced against the token.
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            })
        });
        // Wait for the queued → running transition, then cancel.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if center.snapshot(&snapshot.id).map(|job| job.state) == Some(JobState::Running) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job running");
        assert!(center.cancel(&snapshot.id));
        assert_eq!(
            center.snapshot(&snapshot.id).map(|job| job.state),
            Some(JobState::Cancelled)
        );
        let frames = drain(&mut rx);
        assert!(frames.iter().any(|frame| frame.event == JOB_CHANGED_EVENT));
        assert!(!center.cancel(&snapshot.id));
    }

    #[tokio::test]
    async fn identical_active_jobs_dedupe() {
        let (center, _rx) = center();
        let vault = vault();
        let first = center.spawn(
            JobSpec::new(JobKind::Import, &vault).expect("spec"),
            |_handle| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(())
                })
            },
        );
        let second = center.spawn(
            JobSpec::new(JobKind::Import, &vault).expect("spec"),
            |_handle| Box::pin(async { Ok(()) }),
        );
        assert_eq!(first.id, second.id);
        assert_eq!(center.list(None, None).len(), 1);
    }
}
