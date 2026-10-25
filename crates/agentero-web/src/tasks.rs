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
//! Two execution models share this registry:
//!
//! * **host runner** — [`TaskCenter::spawn`] runs a Rust future (ParseBody /
//!   ParseRefs / DownloadAssets / …);
//! * **renderer offer** — [`TaskCenter::spawn_renderer`] registers the job as
//!   `running` and the browser executes it through the desktop
//!   `job:offer` → `job_report` protocol (`renderer_offer_runner` parity).
//!   A background watcher settles unreported offers as failed after the
//!   configured budget (`AGENTERO_WEB_JOB_OFFER_TIMEOUT_SECS`, default 15 min).
//!
//! Cancellation is cooperative: `job_cancel` fires the job's
//! [`CancellationToken`], flips the snapshot to `cancelled` immediately (like
//! the desktop JobCenter), wakes renderer waiters, and installs a process-wide
//! probe so `agentero-core` pollers (`check_task_not_cancelled`) can stop early.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;

pub const JOB_CHANGED_EVENT: &str = "job:changed";
pub const JOB_COMPLETED_EVENT: &str = "job:completed";
pub const JOB_FAILED_EVENT: &str = "job:failed";
/// Renderer-executed job offer, emitted to the requesting tab (desktop contract).
pub const JOB_OFFER_EVENT: &str = "job:offer";

/// Default renderer report budget: the browser must report a terminal state
/// within 15 minutes, like the desktop `RENDERER_JOB_TIMEOUT`.
pub const DEFAULT_JOB_OFFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Environment override for the renderer report budget (seconds).
pub const ENV_JOB_OFFER_TIMEOUT_SECS: &str = "AGENTERO_WEB_JOB_OFFER_TIMEOUT_SECS";

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

/// Execution host of a job (`ExecHost` in bindings.ts).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ExecHost {
    #[default]
    Host,
    Renderer,
}

/// Execution host of a kind — mirror of the desktop `JobKind::exec_host`.
///
/// Renderer kinds are executed by the browser through the `job:offer` /
/// `job_report` protocol; every other kind runs a Rust runner in the
/// TaskCenter.
pub fn exec_host(kind: JobKind) -> ExecHost {
    match kind {
        JobKind::LayoutAnalyze
        | JobKind::Import
        | JobKind::ConnectorSync
        | JobKind::CitingScan
        | JobKind::LibraryIo
        | JobKind::MetadataRefresh => ExecHost::Renderer,
        _ => ExecHost::Host,
    }
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
    /// Overrides the kind-derived execution host (server-side fallback paths).
    pub host: Option<ExecHost>,
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
            host: None,
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

    /// Force the snapshot's execution host (e.g. a kind the desktop executes in
    /// the renderer, but which the server runs itself in fallback mode).
    pub fn host(mut self, host: ExecHost) -> Self {
        self.host = Some(host);
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
            host: self.host.unwrap_or_else(|| exec_host(self.kind)),
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

    /// Merge a result patch into the running job's `params`.
    ///
    /// The terminal `job:changed` snapshot carries the merged params, so a
    /// renderer reads job results (e.g. smart-highlight suggestions) from the
    /// same snapshot it already listens to — desktop
    /// `JobCenter::merge_running_job_params` parity.
    pub fn merge_params(&self, patch: Value) {
        self.center.merge_running_job_params(&self.id, patch);
    }

    /// Cooperative cancellation token for long-running domain work that needs
    /// to abort between network calls (mirrors the desktop runner contract).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
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
    /// Completion signal for renderer offers (terminal report / cancel).
    settle: Option<Arc<Notify>>,
}

/// Outcome of registering a renderer-executed job.
#[derive(Debug, Clone)]
pub struct RendererJob {
    pub snapshot: JobSnapshot,
    /// `false` when an identical active job was reused (caller must not re-offer).
    pub created: bool,
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
    /// Renderer report budget in milliseconds (env-overridable, test-overridable).
    renderer_timeout_ms: AtomicU64,
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
                renderer_timeout_ms: AtomicU64::new(
                    renderer_timeout_from_env().as_millis().max(1) as u64,
                ),
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
                    settle: None,
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

    /// Override the renderer report budget (tests / embedders).
    pub fn with_renderer_timeout(self, timeout: Duration) -> Self {
        self.inner
            .renderer_timeout_ms
            .store(timeout.as_millis().max(1) as u64, Ordering::Relaxed);
        self
    }

    fn renderer_timeout(&self) -> Duration {
        Duration::from_millis(
            self.inner
                .renderer_timeout_ms
                .load(Ordering::Relaxed)
                .max(1),
        )
    }

    /// Register a renderer-executed job (desktop `renderer_offer_runner` parity).
    ///
    /// The job is `running` immediately; the caller emits `job:offer` with the
    /// returned snapshot, the browser executes it, and the terminal state arrives
    /// through [`TaskCenter::report`] (or [`TaskCenter::cancel`]). A background
    /// watcher settles jobs that never report as failed with
    /// `"renderer job report timeout"`.
    pub fn spawn_renderer(&self, spec: JobSpec) -> RendererJob {
        let id = self.next_id();
        let mut snapshot = spec.snapshot(id.clone());
        snapshot.state = JobState::Running;
        snapshot.phase = Some("running".to_string());
        snapshot.progress = Some(0.0);
        let cancel = CancellationToken::new();
        let settle = Arc::new(Notify::new());
        {
            let mut state = self.lock();
            // Dedupe against an identical active job, like the desktop center:
            // a re-enqueue while the browser is still executing it is a no-op.
            if let Some(existing) = state.active.get(&snapshot.fingerprint).and_then(|id| {
                state
                    .jobs
                    .get(id)
                    .map(|job| (id.clone(), job.snapshot.clone()))
            }) {
                if !existing.1.state.is_terminal() {
                    return RendererJob {
                        snapshot: existing.1,
                        created: false,
                    };
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
                    settle: Some(settle.clone()),
                },
            );
            state.order.push(snapshot.id.clone());
        }
        self.emit_changed(&snapshot);

        let center = self.clone();
        let job_id = snapshot.id.clone();
        let timeout = self.renderer_timeout();
        tokio::spawn(async move {
            center.watch_renderer(job_id, cancel, settle, timeout).await;
        });
        RendererJob {
            snapshot,
            created: true,
        }
    }

    /// Await a renderer job's terminal state: terminal report, cancel, or timeout.
    async fn watch_renderer(
        self,
        job_id: String,
        cancel: CancellationToken,
        settle: Arc<Notify>,
        timeout: Duration,
    ) {
        let started = tokio::time::Instant::now();
        loop {
            // Register the waiter before reading the state: a terminal report
            // racing this check leaves a Notify permit that wakes the select.
            let notified = settle.notified();
            let state = self.snapshot(&job_id).map(|snapshot| snapshot.state);
            match state {
                None => return,
                Some(state) if state.is_terminal() => return,
                Some(_) => {}
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                log::warn!("renderer job {job_id}: report timeout after {}s", timeout.as_secs());
                self.finish(
                    &job_id,
                    JobOutcome::Failed(format!(
                        "renderer job report timeout after {}s",
                        timeout.as_secs()
                    )),
                );
                return;
            }
            tokio::select! {
                _ = notified => {}
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(remaining) => {}
            }
        }
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
                if let Some(notify) = &job.settle {
                    // Wake the renderer watcher immediately (desktop parity:
                    // cancel must not wait for the next poll tick).
                    notify.notify_waiters();
                }
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

    /// Apply a renderer-executor report (`job_report`).
    ///
    /// Progress/phase updates only touch a running row; a terminal state is
    /// applied at most once. Late or duplicate reports (the browser may retry
    /// after a reload) are accepted idempotently and return the current
    /// snapshot; only unknown job ids return `None`.
    pub fn report(
        &self,
        job_id: &str,
        progress: Option<f32>,
        phase: Option<String>,
        error: Option<String>,
        state: Option<JobState>,
    ) -> Option<JobSnapshot> {
        let (snapshot, terminal_event) = {
            let mut guard = self.lock();
            let Some(job) = guard.jobs.get_mut(job_id) else {
                return None;
            };
            if job.snapshot.state != JobState::Running {
                // Settled (or never started): idempotent late report.
                return Some(job.snapshot.clone());
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
            let mut terminal_event = None;
            if let Some(next) = state {
                job.snapshot.state = next;
                if next.is_terminal() {
                    let error = job.snapshot.error.clone();
                    match next {
                        JobState::Succeeded => {
                            job.snapshot.progress = Some(100.0);
                            job.snapshot.phase = Some("completed".to_string());
                            job.snapshot.error = None;
                            terminal_event = Some(JOB_COMPLETED_EVENT);
                        }
                        JobState::Failed => {
                            job.snapshot.phase = Some("failed".to_string());
                            job.snapshot.error = error;
                            terminal_event = Some(JOB_FAILED_EVENT);
                        }
                        JobState::Cancelled => {
                            job.snapshot.progress = None;
                            job.snapshot.phase = Some("cancelled".to_string());
                        }
                        JobState::Skipped => {
                            job.snapshot.progress = None;
                            job.snapshot.phase = Some("skipped".to_string());
                        }
                        JobState::Queued | JobState::Running => {}
                    }
                }
            }
            let snapshot = job.snapshot.clone();
            let active_key = job.active_key.clone();
            if let Some(notify) = &job.settle {
                notify.notify_waiters();
            }
            if terminal_event.is_some() {
                if guard.active.get(&active_key) == Some(&snapshot.id) {
                    guard.active.remove(&active_key);
                }
                guard.cancelled.remove(&snapshot.id);
            }
            (snapshot, terminal_event)
        };
        self.emit_changed(&snapshot);
        if let Some(event) = terminal_event {
            let error = if event == JOB_FAILED_EVENT {
                snapshot.error.clone()
            } else {
                None
            };
            self.emit_terminal(&snapshot, event, error);
        }
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
        // Hold watcher-driven refresh frames for the whole job: a task writes
        // its files in stages, and one renderer refresh per stage is the
        // "auto refresh" storm seen while importing / downloading assets. The
        // watcher flushes every deferred path in one payload once this burst
        // closes (see `events::WriteBurstTracker`).
        let _write_burst = self.inner.bus.write_burst().guard();
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

    /// Merge a patch into a running job's `params` (desktop
    /// `JobCenter::merge_running_job_params` parity).
    ///
    /// Terminal snapshots clone the params, so the following `job:changed` /
    /// `job:completed` events expose the merged result without a second
    /// registry update. Not emitted immediately: while the job is still
    /// running no renderer consumes intermediate params.
    fn merge_running_job_params(&self, id: &str, patch: Value) {
        let mut state = self.lock();
        let Some(job) = state.jobs.get_mut(id) else {
            return;
        };
        if job.snapshot.state != JobState::Running {
            return;
        }
        let mut params = job.snapshot.params.take().unwrap_or_else(|| json!({}));
        match (params.as_object_mut(), patch.as_object()) {
            (Some(dst), Some(src)) => {
                for (key, value) in src {
                    dst.insert(key.clone(), value.clone());
                }
            }
            _ => params = patch,
        }
        job.snapshot.params = Some(params);
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

fn renderer_timeout_from_env() -> Duration {
    std::env::var(ENV_JOB_OFFER_TIMEOUT_SECS)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_JOB_OFFER_TIMEOUT)
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

    #[tokio::test]
    async fn renderer_offer_reports_progress_and_completes() {
        let (center, mut rx) = center();
        let center = center.with_renderer_timeout(Duration::from_secs(30));
        let spec = JobSpec::new(JobKind::Import, &vault())
            .expect("spec")
            .paper_path("papers")
            .params(Some(serde_json::json!({ "mode": "lookup", "text": "1706.03762" })));
        let offered = center.spawn_renderer(spec);
        assert!(offered.created);
        assert_eq!(offered.snapshot.state, JobState::Running);
        assert_eq!(offered.snapshot.host, ExecHost::Renderer);
        assert_eq!(offered.snapshot.paper_path.as_deref(), Some("papers"));
        // The job is listable while the browser executes it (browser boot
        // re-claims running offers through `job_list`).
        assert_eq!(center.list(None, None).len(), 1);

        let progress = center
            .report(
                &offered.snapshot.id,
                Some(42.0),
                Some("fetching".to_string()),
                None,
                None,
            )
            .expect("progress report");
        assert_eq!(progress.state, JobState::Running);
        assert_eq!(progress.progress, Some(42.0));
        assert_eq!(progress.phase.as_deref(), Some("fetching"));

        let done = center
            .report(
                &offered.snapshot.id,
                Some(100.0),
                None,
                None,
                Some(JobState::Succeeded),
            )
            .expect("terminal report");
        assert_eq!(done.state, JobState::Succeeded);
        assert_eq!(done.progress, Some(100.0));
        assert_eq!(done.phase.as_deref(), Some("completed"));

        // Late duplicate report is accepted idempotently and cannot move a
        // settled job back to running.
        let late = center
            .report(&offered.snapshot.id, Some(10.0), None, None, Some(JobState::Failed))
            .expect("late report");
        assert_eq!(late.state, JobState::Succeeded);
        assert_eq!(late.progress, Some(100.0));

        let frames = drain(&mut rx);
        assert!(frames.iter().any(|frame| frame.event == JOB_CHANGED_EVENT));
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame.event == JOB_COMPLETED_EVENT)
                .count(),
            1,
            "terminal lifecycle event must be emitted exactly once"
        );
        assert!(!frames.iter().any(|frame| frame.event == JOB_FAILED_EVENT));
    }

    #[tokio::test]
    async fn renderer_offer_times_out_when_unreported() {
        let (center, mut rx) = center();
        let center = center.with_renderer_timeout(Duration::from_millis(150));
        let offered = center.spawn_renderer(
            JobSpec::new(JobKind::LayoutAnalyze, &vault())
                .expect("spec")
                .paper_path("papers/a"),
        );
        let settled = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(snapshot) = center.snapshot(&offered.snapshot.id) {
                    if snapshot.state.is_terminal() {
                        break snapshot;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("watchdog settles the unreported offer");
        assert_eq!(settled.state, JobState::Failed);
        assert!(
            settled
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("renderer job report timeout"),
            "unexpected timeout error: {:?}",
            settled.error
        );
        let frames = drain(&mut rx);
        assert!(frames.iter().any(|frame| frame.event == JOB_FAILED_EVENT));
    }

    #[tokio::test]
    async fn renderer_offer_cancel_settles_and_dedupes() {
        let (center, _rx) = center();
        let center = center.with_renderer_timeout(Duration::from_secs(30));
        let spec = JobSpec::new(JobKind::CitingScan, &vault()).expect("spec");
        let first = center.spawn_renderer(spec.clone());
        // Re-enqueueing while the browser still runs it reuses the active job.
        let reused = center.spawn_renderer(spec);
        assert!(!reused.created);
        assert_eq!(reused.snapshot.id, first.snapshot.id);
        assert_eq!(center.list(None, None).len(), 1);

        assert!(center.cancel(&first.snapshot.id));
        let cancelled = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = center.snapshot(&first.snapshot.id).map(|snapshot| snapshot.state);
                if state == Some(JobState::Cancelled) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(cancelled.is_ok(), "watcher must observe the cancellation");
        // A cancelled job cannot be cancelled twice and reports stay idempotent.
        assert!(!center.cancel(&first.snapshot.id));
        assert_eq!(
            center
                .report(
                    &first.snapshot.id,
                    None,
                    None,
                    None,
                    Some(JobState::Succeeded)
                )
                .map(|snapshot| snapshot.state),
            Some(JobState::Cancelled)
        );
    }

}
