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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify_debouncer_full::notify::event::{ModifyKind, RenameMode};
use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, FileIdMap,
};
use serde::{Deserialize, Serialize};

use agentero_core::features::paper::capabilities::CapsCache;

use crate::events::EventBus;

/// Trailing quiet window before a batch of FS events is emitted to the UI.
const DEBOUNCE: Duration = Duration::from_millis(300);

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
}

#[derive(Default)]
struct Watchers {
    /// Canonical Vault root → reference-counted watcher.
    by_root: HashMap<PathBuf, WatchEntry>,
    /// Tab label → canonical Vault root it is watching (one Vault per tab).
    by_label: HashMap<String, PathBuf>,
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

        let mut debouncer = Some(self.spawn_watcher(&key)?);
        let mut watchers = self.lock()?;
        if let Some(entry) = watchers.by_root.get_mut(&key) {
            // Lost a creation race with another request; reuse its watcher.
            entry.labels.insert(label.to_string());
        } else {
            watchers.by_root.insert(
                key.clone(),
                WatchEntry {
                    labels: HashSet::from([label.to_string()]),
                    debouncer: debouncer.take(),
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

    fn spawn_watcher(&self, root: &Path) -> Result<WatchDebouncer, String> {
        let bus = self.bus.clone();
        let caps = self.caps.clone();
        let inner = self.inner.clone();
        let watched_root = root.to_path_buf();

        let callback = move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for error in errors {
                        log::warn!(
                            "vault watcher error root={}: {error}",
                            watched_root.display()
                        );
                    }
                    return;
                }
            };
            let payloads = payloads_from_events(events);
            if payloads.is_empty() {
                return;
            }
            let labels: Vec<String> = match inner.lock() {
                Ok(watchers) => watchers
                    .by_root
                    .get(&watched_root)
                    .map(|entry| entry.labels.iter().cloned().collect())
                    .unwrap_or_default(),
                Err(_) => return,
            };
            if labels.is_empty() {
                return;
            }
            for payload in &payloads {
                invalidate_caps(&caps, &watched_root, &payload.paths);
                for label in &labels {
                    bus.emit_to(label, EVENT_FILE_CHANGED, payload);
                }
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
        Ok(debouncer)
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

/// Convert a debounced notify batch into one payload per event kind, dropping
/// ignored paths and collapsing atomic-write temp churn into a modify.
fn payloads_from_events(events: Vec<DebouncedEvent>) -> Vec<FileChangedPayload> {
    let mut out: Vec<FileChangedPayload> = Vec::new();
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
        let kind = if from_atomic_write {
            "modify"
        } else {
            kind_label(&event.kind)
        };
        let rename = if from_atomic_write {
            None
        } else {
            verified_rename_pair(&event.kind, &paths)
        };
        out.push(FileChangedPayload {
            paths,
            kind: kind.to_string(),
            rename,
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
    fn caps_invalidation_targets_paper_folders() {
        assert!(is_caps_relevant("/vault/papers/a/PAPER.md"));
        assert!(is_caps_relevant("/vault/papers/a/source/main.TeX"));
        assert!(!is_caps_relevant("/vault/papers/a/NOTES.md"));
    }
}
