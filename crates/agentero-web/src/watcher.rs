//! Multi-tab Vault filesystem watcher.
//!
//! One debounced `notify` watcher per canonical Vault root, reference-counted
//! by browser-tab label (`fs_watch_start` / `fs_watch_stop`). Each debounced
//! batch is translated into the desktop `vault:file-changed` payload
//! (`{ paths, kind, rename? }`, absolute paths) and emitted with
//! `target = <tab label>`, so a tab only reacts to the Vault it opened
//! (docs/development/webui.md §2.3).
//!
//! Semantics mirror `src-tauri/src/features/vault/watcher/mod.rs`:
//! * trailing 300 ms debounce (notify-debouncer-full, 300 ms timeout);
//! * only a complete `RenameMode::Both` pair is a trustworthy rename — every
//!   other structural event refreshes the tree but never authorizes a Vault
//!   rewrite;
//! * atomic-write temps (`.agentero-rename-*`) count as content modifies;
//! * internal state (`.agentero/`), VCS metadata (`.git/`), dependencies
//!   (`node_modules/`) and mirrored sync-store artifacts are ignored.
//!
//! Headless difference: one debounced batch is emitted as **one** payload for
//! all non-rename events (every touched path in `paths`), not one payload per
//! OS event. A server task materializing a paper (PDF + TeX sources + NOTES +
//! layout + marks …) writes dozens of files per batch; per-event frames made
//! the renderer run its whole refresh chain once per file. Verified rename
//! pairs stay separate payloads because the renderer repairs internal links
//! from a single `{from,to}` pair.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::event::{ModifyKind, RenameMode};
use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, FileIdMap,
};
use serde::{Deserialize, Serialize};

use agentero_core::features::paper::capabilities::CapsCache;

use crate::events::EventBus;

/// Trailing quiet window before a batch of FS events is collected.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Quiet window after the last collected batch before deferred frames are
/// emitted. Together with [`DEBOUNCE`] this merges the staged writes of one
/// task into a single renderer refresh.
const DEFERRED_SETTLE: Duration = Duration::from_millis(150);

/// While a server-side write burst keeps running, emit held frames at least
/// this often so a long download never freezes the UI until it ends.
const DEFERRED_MAX_HOLD: Duration = Duration::from_millis(2500);

/// Poll interval of the per-watcher deferred-frame flusher thread.
const DEFERRED_TICK: Duration = Duration::from_millis(50);

/// Event name, unchanged from the desktop Host.
pub const EVENT_FILE_CHANGED: &str = "vault:file-changed";

/// Payload for the `vault:file-changed` event (consumed by the renderer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangedPayload {
    /// Absolute paths touched by this (debounced) batch.
    pub paths: Vec<String>,
    /// Coarse change kind: `"create" | "modify" | "remove" | "other"`.
    pub kind: String,
    /// Present only when the OS delivered one trustworthy old/new rename pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<FileRename>,
}

/// A rename pair emitted by the native watcher. `None` means the watcher did
/// not preserve enough information for automatic internal-link repair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileRename {
    pub from: String,
    pub to: String,
}

type WatchDebouncer = Debouncer<notify_debouncer_full::notify::RecommendedWatcher, FileIdMap>;

struct WatchEntry {
    /// Browser-tab labels currently watching this root.
    labels: HashSet<String>,
    /// Dropped (and its thread joined) when the last label stops watching.
    debouncer: Option<WatchDebouncer>,
    /// Frames held until the current write burst settles (shared with the
    /// flusher thread).
    deferred: Arc<Mutex<DeferredFrames>>,
    /// Set when the entry is retired so the flusher thread exits.
    flush_stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct Watchers {
    /// Canonical Vault root → reference-counted watcher.
    by_root: HashMap<PathBuf, WatchEntry>,
    /// Tab label → canonical Vault root it is watching (one Vault per tab).
    by_label: HashMap<String, PathBuf>,
}

/// Frames of one watcher root waiting for the write burst to settle.
///
/// A server task writes its files in stages. Each 300 ms debounce batch used to
/// reach the renderer immediately, which then ran a full refresh per stage — the
/// "auto refresh" storm while importing or downloading assets. Frames are held
/// while a [`crate::events::WriteBurstTracker`] burst is in flight and for a
/// short quiet window after the last batch, then emitted as one merged payload.
#[derive(Default)]
struct DeferredFrames {
    payloads: Vec<FileChangedPayload>,
    /// Arrival time of the newest held batch (settle-window anchor).
    last_event_at: Option<Instant>,
    /// Arrival time of the oldest held batch (max-hold anchor).
    first_event_at: Option<Instant>,
}

impl DeferredFrames {
    fn push(&mut self, payloads: Vec<FileChangedPayload>, now: Instant) {
        if payloads.is_empty() {
            return;
        }
        self.first_event_at.get_or_insert(now);
        self.last_event_at = Some(now);
        self.payloads.extend(payloads);
    }

    fn is_empty(&self) -> bool {
        self.payloads.is_empty()
    }

    /// True when the held frames should be emitted now: the write burst is
    /// over and the filesystem went quiet, or the max-hold cap tripped.
    fn ready(&self, write_burst_active: bool, now: Instant) -> bool {
        let Some(last) = self.last_event_at else {
            return false;
        };
        let max_held = self
            .first_event_at
            .is_some_and(|first| now.duration_since(first) >= DEFERRED_MAX_HOLD);
        max_held || (!write_burst_active && now.duration_since(last) >= DEFERRED_SETTLE)
    }

    fn take_merged(&mut self) -> Vec<FileChangedPayload> {
        self.last_event_at = None;
        self.first_event_at = None;
        merge_payloads(std::mem::take(&mut self.payloads))
    }
}

/// Merge several already-batched payloads into the fewest equivalent frames.
/// Verified renames keep their own payload (one `{from,to}` pair each); every
/// other path merges into one payload, deduped, and a mixed-kind batch becomes
/// `"other"` so the renderer still treats it as structural.
fn merge_payloads(payloads: Vec<FileChangedPayload>) -> Vec<FileChangedPayload> {
    let mut renames: Vec<FileChangedPayload> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut kinds: HashSet<String> = HashSet::new();

    for payload in payloads {
        if payload.rename.is_some() {
            renames.push(payload);
            continue;
        }
        kinds.insert(payload.kind);
        for path in payload.paths {
            if seen_paths.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    let mut out = renames;
    if !paths.is_empty() {
        let kind = if kinds.len() == 1 {
            kinds
                .into_iter()
                .next()
                .unwrap_or_else(|| "other".to_string())
        } else {
            "other".to_string()
        };
        out.push(FileChangedPayload {
            paths,
            kind,
            rename: None,
        });
    }
    out
}

/// Process-wide multi-tab Vault watcher registry.
///
/// Cheap to clone (one `Arc`): `AppState` owns the service and command
/// handlers take a clone without lifetime ceremony.
#[derive(Clone)]
pub struct FsWatchService {
    bus: EventBus,
    caps: CapsCache,
    inner: Arc<Mutex<Watchers>>,
}

impl FsWatchService {
    pub fn new(bus: EventBus, caps: CapsCache) -> Self {
        Self {
            bus,
            caps,
            inner: Arc::new(Mutex::new(Watchers::default())),
        }
    }

    /// Start (or restart) watching `vault_path` for `label`. Other tabs already
    /// watching the same root are unaffected; the last `stop` drops the
    /// OS-level watcher.
    pub fn start(&self, label: &str, vault_path: &str) -> Result<(), String> {
        let root = resolve_vault_root(vault_path)?;
        let key = agentero_core::fs::canonicalize_best_effort(&root);

        // Same Vault already watched by this tab: nothing to do.
        {
            let watchers = self.lock()?;
            if watchers.by_label.get(label) == Some(&key) {
                return Ok(());
            }
        }
        // Re-pointing a tab at another Vault: drop its previous watcher first.
        self.stop(label);

        // Fast path: another tab already watches this root — just count us in.
        {
            let mut watchers = self.lock()?;
            if let Some(entry) = watchers.by_root.get_mut(&key) {
                entry.labels.insert(label.to_string());
                watchers.by_label.insert(label.to_string(), key);
                return Ok(());
            }
        }

        let (debouncer, deferred, flush_stop) = self.spawn_watcher(&key)?;
        let mut debouncer = Some(debouncer);
        let mut watchers = self.lock()?;
        if let Some(entry) = watchers.by_root.get_mut(&key) {
            // Lost a creation race with another request; reuse its watcher and
            // retire the one we just spawned (its flusher thread included).
            entry.labels.insert(label.to_string());
            flush_stop.store(true, Ordering::Relaxed);
        } else {
            watchers.by_root.insert(
                key.clone(),
                WatchEntry {
                    labels: HashSet::from([label.to_string()]),
                    debouncer: debouncer.take(),
                    deferred,
                    flush_stop,
                },
            );
        }
        watchers.by_label.insert(label.to_string(), key);
        // Drop a racing watcher *outside* the registry lock: dropping joins the
        // debouncer thread, whose callback also locks the registry.
        drop(watchers);
        drop(debouncer);
        Ok(())
    }

    /// Stop watching for `label` (no-op if it was not watching). Drops the
    /// OS-level watcher when it was the last tab on that Vault.
    pub fn stop(&self, label: &str) {
        let mut to_drop: Option<WatchDebouncer> = None;
        {
            let Ok(mut watchers) = self.inner.lock() else {
                return;
            };
            let Some(root) = watchers.by_label.remove(label) else {
                return;
            };
            let empty = match watchers.by_root.get_mut(&root) {
                Some(entry) => {
                    entry.labels.remove(label);
                    entry.labels.is_empty()
                }
                None => false,
            };
            if empty {
                if let Some(entry) = watchers.by_root.remove(&root) {
                    // Stop the deferred-frame flusher before dropping the
                    // debouncer (whose thread callback locks this registry).
                    entry.flush_stop.store(true, Ordering::Relaxed);
                    to_drop = entry.debouncer;
                }
            }
        }
        // Joining the debouncer thread must not happen under the registry lock.
        drop(to_drop);
    }

    /// Canonical root currently watched by `label`, if any (diagnostics/tests).
    pub fn watched_root(&self, label: &str) -> Option<PathBuf> {
        self.inner
            .lock()
            .ok()
            .and_then(|watchers| watchers.by_label.get(label).cloned())
    }

    /// Number of distinct roots with at least one watcher (diagnostics/tests).
    pub fn watched_roots(&self) -> usize {
        self.inner
            .lock()
            .map(|watchers| watchers.by_root.len())
            .unwrap_or(0)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Watchers>, String> {
        self.inner
            .lock()
            .map_err(|_| "fs watch state lock poisoned".to_string())
    }

    /// Create the OS watcher plus its deferred-frame flusher.
    ///
    /// The debouncer callback only *collects* frames (and invalidates cached
    /// paper capabilities); the flusher thread emits them once the current
    /// server-side write burst settles, so one task costs one renderer refresh
    /// instead of one per debounced batch.
    #[allow(clippy::type_complexity)]
    fn spawn_watcher(
        &self,
        root: &Path,
    ) -> Result<(WatchDebouncer, Arc<Mutex<DeferredFrames>>, Arc<AtomicBool>), String> {
        let bus = self.bus.clone();
        let caps = self.caps.clone();
        let inner = self.inner.clone();
        let watched_root = root.to_path_buf();
        let deferred = Arc::new(Mutex::new(DeferredFrames::default()));

        let callback_deferred = deferred.clone();
        let callback_root = watched_root.clone();
        let callback = move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for error in errors {
                        log::warn!(
                            "vault watcher error root={}: {error}",
                            callback_root.display()
                        );
                    }
                    return;
                }
            };
            let payloads = payloads_from_events(events);
            if payloads.is_empty() {
                return;
            }
            // Capability invalidation is cheap and must track real disk state
            // even while the UI refresh itself is deferred.
            for payload in &payloads {
                invalidate_caps(&caps, &callback_root, &payload.paths);
            }
            if let Ok(mut pending) = callback_deferred.lock() {
                pending.push(payloads, Instant::now());
            }
        };

        let mut debouncer = new_debouncer(DEBOUNCE, None, callback)
            .map_err(|error| format!("vault watcher init failed: {error}"))?;
        debouncer
            .watcher()
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| format!("vault watcher watch failed: {error}"))?;
        // File-id cache powers rename stitching on backends without rename
        // cookies (FSEvents / Windows) — same setup as notify-debouncer-full.
        debouncer.cache().add_root(root, RecursiveMode::Recursive);

        // Flusher: emits the held frames as one payload once the active write
        // burst (task runner / import handler) has settled.
        let flush_stop = Arc::new(AtomicBool::new(false));
        let flusher_stop = flush_stop.clone();
        let flusher_deferred = deferred.clone();
        let flusher_bus = bus.clone();
        let flusher_inner = inner.clone();
        let flusher_root = watched_root.clone();
        thread::Builder::new()
            .name("vault-watch-flusher".to_string())
            .spawn(move || {
                while !flusher_stop.load(Ordering::Relaxed) {
                    thread::sleep(DEFERRED_TICK);
                    let busy = flusher_bus.write_burst().is_active();
                    let frames = match flusher_deferred.lock() {
                        Ok(mut pending) => {
                            if pending.is_empty() || !pending.ready(busy, Instant::now()) {
                                continue;
                            }
                            pending.take_merged()
                        }
                        Err(_) => continue,
                    };
                    if frames.is_empty() {
                        continue;
                    }
                    let labels: Vec<String> = match flusher_inner.lock() {
                        Ok(watchers) => watchers
                            .by_root
                            .get(&flusher_root)
                            .map(|entry| entry.labels.iter().cloned().collect())
                            .unwrap_or_default(),
                        Err(_) => continue,
                    };
                    if labels.is_empty() {
                        continue;
                    }
                    for payload in &frames {
                        for label in &labels {
                            flusher_bus.emit_to(label, EVENT_FILE_CHANGED, payload);
                        }
                    }
                }
            })
            .map_err(|error| format!("vault watcher flusher init failed: {error}"))?;

        Ok((debouncer, deferred, flush_stop))
    }
}

fn resolve_vault_root(vault_path: &str) -> Result<PathBuf, String> {
    let trimmed = vault_path.trim();
    if trimmed.is_empty() {
        return Err("vaultPath is required".to_string());
    }
    if trimmed.starts_with("remote:") {
        return Err("remote vaults cannot be watched on the server".to_string());
    }
    agentero_core::fs::resolve_vault(trimmed).map_err(|error| error.to_string())
}

/// Ignore churn from internal state, VCS metadata, dependencies, and mirrored
/// sync-store artifacts. Matched as any path segment, mirroring the desktop
/// watcher and `snapshot::is_ignored_name` so the two never disagree.
fn is_ignored(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    // Trailing slash makes endpoint segments (`.git` itself) match too.
    let padded = format!("{normalized}/");
    padded.contains("/.agentero/")
        || padded.contains("/.git/")
        || padded.contains("/node_modules/")
        || padded.split('/').any(|segment| {
            !segment.is_empty()
                && agentero_core::features::vault::tree::is_sync_store_artifact(segment)
        })
}

/// Temp path used by Host `atomic_write` (wiki rename / heading rename).
fn is_agentero_atomic_temp(path: &str) -> bool {
    path.replace('\\', "/").contains(".agentero-rename-")
}

fn kind_label(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Remove(_) => "remove",
        EventKind::Modify(ModifyKind::Name(_)) => "rename",
        EventKind::Modify(_) => "modify",
        _ => "other",
    }
}

/// Only `RenameMode::Both` with exactly one old/new pair is trusted.
fn verified_rename_pair(kind: &EventKind, paths: &[String]) -> Option<FileRename> {
    if !matches!(kind, EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
        || paths.len() != 2
        || paths[0] == paths[1]
    {
        return None;
    }
    Some(FileRename {
        from: paths[0].clone(),
        to: paths[1].clone(),
    })
}

/// Convert a debounced notify batch into renderer payloads, dropping ignored
/// paths and collapsing atomic-write temp churn into a modify.
///
/// All non-rename events in the batch merge into a single payload: a task that
/// materializes a paper folder writes dozens of files, and one frame per file
/// made the renderer re-read disk / re-list the tree / rescan the catalog once
/// per file. Duplicate paths are collapsed, and the merged `kind` is the
/// single shared kind when every merged event agrees, `"other"` when the batch
/// mixes structural kinds (the renderer treats anything but `"modify"` as
/// structural).
///
/// A trustworthy rename pair keeps its own payload: internal-link repair needs
/// the one `{from,to}` pair, and a batch can contain several unrelated renames.
fn payloads_from_events(events: Vec<DebouncedEvent>) -> Vec<FileChangedPayload> {
    let mut renames: Vec<FileChangedPayload> = Vec::new();
    let mut merged_paths: Vec<String> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    // Kinds of the merged (non-rename) events in this batch.
    let mut merged_kinds: HashSet<&'static str> = HashSet::new();

    for debounced in events {
        let event = debounced.event;
        let raw_paths: Vec<String> = event
            .paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .filter(|path| !is_ignored(path))
            .collect();
        if raw_paths.is_empty() {
            continue;
        }
        // Content replace via tmp+rename is not a user/wiki path rename.
        let from_atomic_write = raw_paths.iter().any(|path| is_agentero_atomic_temp(path));
        let paths: Vec<String> = raw_paths
            .into_iter()
            .filter(|path| !is_agentero_atomic_temp(path))
            .collect();
        if paths.is_empty() {
            continue;
        }
        if !from_atomic_write {
            if let Some(rename) = verified_rename_pair(&event.kind, &paths) {
                renames.push(FileChangedPayload {
                    paths,
                    kind: "rename".to_string(),
                    rename: Some(rename),
                });
                continue;
            }
        }
        let kind = if from_atomic_write {
            "modify"
        } else {
            kind_label(&event.kind)
        };
        merged_kinds.insert(kind);
        for path in paths {
            if seen_paths.insert(path.clone()) {
                merged_paths.push(path);
            }
        }
    }

    let mut out = renames;
    if !merged_paths.is_empty() {
        let kind = match merged_kinds.len() {
            1 => merged_kinds
                .iter()
                .next()
                .copied()
                .unwrap_or("other")
                .to_string(),
            _ => "other".to_string(),
        };
        out.push(FileChangedPayload {
            paths: merged_paths,
            kind,
            rename: None,
        });
    }
    out
}

/// Files whose presence decides `PaperCaps`: a PDF, LaTeX source that
/// supersedes liteparse, or an existing `PAPER.md`.
fn is_caps_relevant(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.ends_with("/paper.md")
        || lower.ends_with(".pdf")
        || lower.ends_with(".tex")
        || lower.ends_with(".ltx")
}

/// Drop cached `PaperCaps` for folders an external write touched, so the next
/// reconcile probes real disk state instead of a stale snapshot.
fn invalidate_caps(caps: &CapsCache, vault_root: &Path, paths: &[String]) {
    for path in paths.iter().filter(|path| is_caps_relevant(path)) {
        let Ok(rel) = Path::new(path).strip_prefix(vault_root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let mut current = rel.as_str();
        // Every ancestor directory up to (and including) the paper folder.
        while let Some((parent, _)) = current.rsplit_once('/') {
            if parent.is_empty() {
                break;
            }
            caps.invalidate(vault_root, parent);
            current = parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_full::notify::event::{CreateKind, DataChange};
    use notify_debouncer_full::notify::Event;

    fn debounced(kind: EventKind, paths: &[&str]) -> DebouncedEvent {
        let mut event = Event::new(kind);
        for path in paths {
            event = event.add_path(PathBuf::from(path));
        }
        DebouncedEvent::from(event)
    }

    #[test]
    fn only_complete_rename_pairs_are_trusted() {
        let kind = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        let paths = vec!["/vault/old.md".to_string(), "/vault/new.md".to_string()];
        let pair = verified_rename_pair(&kind, &paths).expect("trusted pair");
        assert_eq!(pair.from, "/vault/old.md");
        assert_eq!(pair.to, "/vault/new.md");
        assert!(verified_rename_pair(
            &EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            &paths
        )
        .is_none());
        assert!(verified_rename_pair(&kind, &paths[..1]).is_none());
    }

    #[test]
    fn ignored_paths_are_internal_state_vcs_and_dependencies() {
        assert!(is_ignored("/vault/.agentero/catalog.sqlite"));
        // Derived hidden caches (tag index) must never surface as user file events.
        assert!(is_ignored("/vault/.agentero/tag-index.json"));
        assert!(is_ignored("/vault/.git/index"));
        assert!(is_ignored("/vault/node_modules/pkg/a.js"));
        assert!(is_ignored(r"C:\vault\.git\index"));
        assert!(is_ignored("/vault/blobs/ab/hash"));
        assert!(is_ignored("/vault/HEAD"));
        assert!(!is_ignored("/vault/papers/a/NOTES.md"));
        assert!(!is_ignored("/vault/notes/HEAD.md"));
    }

    #[test]
    fn atomic_write_temps_are_content_modifies() {
        let events = vec![debounced(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[
                "/vault/papers/a/NOTES.md.agentero-rename-deadbeef.tmp",
                "/vault/papers/a/NOTES.md",
            ],
        )];
        let payloads = payloads_from_events(events);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].kind, "modify");
        assert_eq!(payloads[0].paths, vec!["/vault/papers/a/NOTES.md"]);
        assert!(payloads[0].rename.is_none());
    }

    #[test]
    fn rename_payload_keeps_the_verified_pair() {
        let events = vec![debounced(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/vault/a.md", "/vault/b.md"],
        )];
        let payloads = payloads_from_events(events);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].kind, "rename");
        let rename = payloads[0].rename.as_ref().expect("rename pair");
        assert_eq!(rename.from, "/vault/a.md");
        assert_eq!(rename.to, "/vault/b.md");
    }

    #[test]
    fn one_batch_of_writes_emits_one_payload_with_every_path() {
        // A task materializing a paper folder writes many files in one batch:
        // the renderer must run its refresh chain once, not once per file.
        let events = vec![
            debounced(
                EventKind::Create(CreateKind::File),
                &["/vault/papers/p/PAPER.md"],
            ),
            debounced(
                EventKind::Create(CreateKind::File),
                &["/vault/papers/p/NOTES.md"],
            ),
            debounced(
                EventKind::Create(CreateKind::File),
                &["/vault/papers/p/source/layout.json"],
            ),
        ];
        let payloads = payloads_from_events(events);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].kind, "create");
        assert_eq!(
            payloads[0].paths,
            vec![
                "/vault/papers/p/PAPER.md",
                "/vault/papers/p/NOTES.md",
                "/vault/papers/p/source/layout.json"
            ]
        );
    }

    #[test]
    fn mixed_batch_kinds_merge_into_one_structural_payload() {
        let events = vec![
            debounced(
                EventKind::Create(CreateKind::File),
                &["/vault/papers/p/PAPER.md"],
            ),
            debounced(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &["/vault/papers/p/NOTES.md"],
            ),
        ];
        let payloads = payloads_from_events(events);
        assert_eq!(payloads.len(), 1);
        // Mixed kinds must stay structural so the tree refreshes.
        assert_eq!(payloads[0].kind, "other");
        assert_eq!(payloads[0].paths.len(), 2);
    }

    #[test]
    fn duplicate_paths_in_one_batch_collapse() {
        let modify = || debounced(EventKind::Modify(ModifyKind::Any), &["/vault/notes/a.md"]);
        let payloads = payloads_from_events(vec![modify(), modify()]);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].paths, vec!["/vault/notes/a.md"]);
        assert_eq!(payloads[0].kind, "modify");
    }

    #[test]
    fn verified_renames_stay_separate_from_merged_writes() {
        let events = vec![
            debounced(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &["/vault/notes/old.md", "/vault/notes/new.md"],
            ),
            debounced(
                EventKind::Create(CreateKind::File),
                &["/vault/papers/p/PAPER.md"],
            ),
        ];
        let payloads = payloads_from_events(events);
        assert_eq!(payloads.len(), 2);
        let rename = payloads
            .iter()
            .find(|payload| payload.kind == "rename")
            .expect("rename payload");
        assert_eq!(rename.paths.len(), 2);
        assert!(rename.rename.is_some());
        let merged = payloads
            .iter()
            .find(|payload| payload.kind == "create")
            .expect("merged payload");
        assert_eq!(merged.paths, vec!["/vault/papers/p/PAPER.md"]);
    }

    fn payload(kind: &str, paths: &[&str]) -> FileChangedPayload {
        FileChangedPayload {
            paths: paths.iter().map(|path| path.to_string()).collect(),
            kind: kind.to_string(),
            rename: None,
        }
    }

    #[test]
    fn deferred_frames_wait_for_the_write_burst_to_settle() {
        let t0 = Instant::now();
        let mut frames = DeferredFrames::default();
        frames.push(vec![payload("create", &["/vault/papers/p/PAPER.md"])], t0);

        // A task is still writing: never emit mid-burst.
        assert!(!frames.ready(true, t0 + DEFERRED_SETTLE));
        // Burst over, but the trailing settle window has not elapsed yet.
        assert!(!frames.ready(false, t0 + DEFERRED_SETTLE / 2));
        assert!(frames.ready(false, t0 + DEFERRED_SETTLE));

        let merged = frames.take_merged();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].paths, vec!["/vault/papers/p/PAPER.md"]);
        assert!(frames.is_empty());
    }

    #[test]
    fn long_write_burst_still_flushes_at_the_max_hold_cap() {
        let t0 = Instant::now();
        let mut frames = DeferredFrames::default();
        frames.push(vec![payload("create", &["/vault/papers/p/a.pdf"])], t0);
        frames.push(
            vec![payload("create", &["/vault/papers/p/b.pdf"])],
            t0 + DEFERRED_MAX_HOLD,
        );
        // Still busy, but the oldest frame has been held for the cap.
        assert!(frames.ready(true, t0 + DEFERRED_MAX_HOLD));
    }

    #[test]
    fn deferred_batches_merge_into_one_payload() {
        let t0 = Instant::now();
        let mut frames = DeferredFrames::default();
        frames.push(
            vec![
                payload("create", &["/vault/papers/p/PAPER.md"]),
                payload("modify", &["/vault/papers/p/NOTES.md"]),
            ],
            t0,
        );
        frames.push(
            vec![payload(
                "create",
                &[
                    "/vault/papers/p/PAPER.md",
                    "/vault/papers/p/source/layout.json",
                ],
            )],
            t0 + DEFERRED_SETTLE / 2,
        );

        let merged = frames.take_merged();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].kind, "other");
        assert_eq!(
            merged[0].paths,
            vec![
                "/vault/papers/p/PAPER.md",
                "/vault/papers/p/NOTES.md",
                "/vault/papers/p/source/layout.json"
            ]
        );
    }

    #[test]
    fn deferred_merge_keeps_verified_renames_separate() {
        let t0 = Instant::now();
        let mut frames = DeferredFrames::default();
        frames.push(
            vec![FileChangedPayload {
                paths: vec!["/vault/a.md".to_string(), "/vault/b.md".to_string()],
                kind: "rename".to_string(),
                rename: Some(FileRename {
                    from: "/vault/a.md".to_string(),
                    to: "/vault/b.md".to_string(),
                }),
            }],
            t0,
        );
        frames.push(vec![payload("create", &["/vault/c.md"])], t0);

        let merged = frames.take_merged();
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|payload| payload.rename.is_some()));
        assert!(merged
            .iter()
            .any(|payload| payload.kind == "create"
                && payload.paths == vec!["/vault/c.md".to_string()]));
    }

    #[test]
    fn caps_invalidation_targets_paper_folders() {
        assert!(is_caps_relevant("/vault/papers/a/PAPER.md"));
        assert!(is_caps_relevant("/vault/papers/a/source/main.TeX"));
        assert!(!is_caps_relevant("/vault/papers/a/NOTES.md"));
    }
}
