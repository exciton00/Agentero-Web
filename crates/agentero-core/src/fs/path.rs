//! Path safety and shared FS metadata types.

use crate::error::AppError;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// Capability flags so UI / business logic can degrade without guesswork.
#[derive(Debug, Clone, Copy, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct FsCaps {
    pub atomic_rename: bool,
    pub reliable_watch: bool,
    pub sqlite_native: bool,
    pub cheap_random_read: bool,
    pub agent_cwd_local: bool,
    pub finder_reveal: bool,
}

impl FsCaps {
    pub const LOCAL: Self = Self {
        atomic_rename: true,
        reliable_watch: true,
        sqlite_native: true,
        cheap_random_read: true,
        agent_cwd_local: true,
        finder_reveal: true,
    };

    pub const REMOTE: Self = Self {
        atomic_rename: true,
        reliable_watch: false,
        sqlite_native: false,
        cheap_random_read: false,
        agent_cwd_local: false,
        finder_reveal: false,
    };
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct FsDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
    /// Vault-relative path using `/`.
    pub path: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsFileMeta {
    pub size: u64,
    /// Modified time as unix seconds when known; 0 if unavailable.
    pub mtime: u64,
    pub is_dir: bool,
    pub is_file: bool,
}

/// Minimal vault-relative cleanup: `\` → `/` and strip leading/trailing `/`.
/// Interior `//`, `.`/`..` segments and whitespace are kept untouched — use for
/// strings that must round-trip against on-disk names or persisted state,
/// where the stronger [`normalize_rel`] would alter legitimate paths.
pub fn normalize_rel_separators(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_string()
}

/// Normalize a vault-relative path to UNIX style without leading `/`.
/// Trims surrounding whitespace, collapses duplicate slashes, and drops `.`
/// segments (but keeps `..`; see [`path_escapes_root`] / [`sanitize_vault_rel`]
/// for escape guards, and [`normalize_rel_lexical`] for `..` resolution).
pub fn normalize_rel(rel: &str) -> String {
    let s = rel.trim().replace('\\', "/");
    let parts: Vec<&str> = s
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    parts.join("/")
}

/// Lexically normalized vault-relative path: `\` → `/`, drop empty and `.`
/// segments, and resolve each `..` against the preceding segment (a `..` with
/// nothing to pop is kept). Unlike [`normalize_rel`] it does not trim
/// whitespace — wiki link targets may contain meaningful spaces.
pub fn normalize_rel_lexical(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    parts.push("..");
                }
            }
            value => parts.push(value),
        }
    }
    parts.join("/")
}

/// Returns true if `rel` attempts to escape the vault root via `..` segments.
pub fn path_escapes_root(rel: &str) -> bool {
    let norm = normalize_rel(rel);
    if norm.is_empty() {
        return false;
    }
    for part in norm.split('/') {
        if part == ".." {
            return true;
        }
    }
    false
}

/// True when `rel` is usable as a vault-relative path: not absolute and built
/// only from `Normal` / `CurDir` components (rejects `..`, `/` roots and
/// Windows prefixes). Empty input passes — callers that require a non-empty
/// path check that separately.
pub fn safe_relative_path(rel: &str) -> bool {
    let path = Path::new(rel);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Normalize a vault-relative path and reject empty or root-escaping inputs.
/// The canonical guard for user/IPC-provided paper/file paths.
pub fn sanitize_vault_rel(rel: &str) -> Result<String, String> {
    let norm = normalize_rel(rel);
    if norm.is_empty() {
        return Err("empty vault path".into());
    }
    if norm.split('/').any(|p| p == "..") {
        return Err("path escapes vault root".into());
    }
    Ok(norm)
}

/// Join remote root (absolute remote path) with vault-relative path.
pub fn join_remote(remote_root: &str, rel: &str) -> Result<String, String> {
    if path_escapes_root(rel) {
        return Err("path escapes vault root".into());
    }
    let root = remote_root.trim_end_matches('/');
    let rel = normalize_rel(rel);
    if rel.is_empty() {
        Ok(if root.is_empty() {
            "/".into()
        } else {
            root.to_string()
        })
    } else if root.is_empty() {
        Ok(format!("/{rel}"))
    } else {
        Ok(format!("{root}/{rel}"))
    }
}

/// Trim + validate that `vault_path` points at an existing directory.
pub fn resolve_vault(vault_path: &str) -> Result<PathBuf, AppError> {
    let vault = PathBuf::from(vault_path.trim());
    ensure_vault_dir(&vault)?;
    Ok(vault)
}

/// Best-effort canonicalization used to key vault roots (job dedup, capability
/// caches): returns the canonical absolute path, or the input unchanged when
/// `canonicalize` fails (missing directory, permissions, exotic filesystems).
pub fn canonicalize_best_effort(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Vault-relative form of `raw`, or `None` when it escapes the vault.
///
/// Absolute paths may use another spelling of the same directory: a symlink
/// (`/var` vs `/private/var`) or the Windows `\\?\` prefix. A relative `raw`
/// is only lexically normalized.
pub fn vault_relative(vault_root: &Path, raw: &str) -> Option<String> {
    let root_owned = vault_root.to_string_lossy();
    let root = Path::new(without_verbatim_prefix(root_owned.as_ref()));
    let raw_plain = without_verbatim_prefix(raw);
    let file = Path::new(raw_plain);
    let slash = raw_plain.replace('\\', "/");
    let rel = if file.is_absolute() || slash.starts_with('/') {
        strip_under_vault(root, file)?
    } else {
        slash
    };
    let rel = normalize_rel_lexical(&rel);
    if rel.split('/').any(|part| part == "..") {
        None
    } else {
        Some(rel)
    }
}

/// `\\?\D:\...` → `D:\...`. UNC and device paths keep the prefix.
fn without_verbatim_prefix(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest,
        _ => path,
    }
}

fn strip_under_vault(root: &Path, file: &Path) -> Option<String> {
    fn rel_string(rel: &Path) -> String {
        rel.to_string_lossy().replace('\\', "/")
    }
    if let Ok(rel) = file.strip_prefix(root) {
        return Some(rel_string(rel));
    }
    let canonical_root = canonicalize_best_effort(root);
    if canonical_root.as_path() != root {
        if let Ok(rel) = file.strip_prefix(&canonical_root) {
            return Some(rel_string(rel));
        }
    }
    let canonical_file = canonicalize_best_effort(file);
    if canonical_file.as_path() == file {
        return None;
    }
    canonical_file
        .strip_prefix(&canonical_root)
        .or_else(|_| canonical_file.strip_prefix(root))
        .ok()
        .map(rel_string)
}

/// Validate that `root` is an existing directory — shared vault guard for
/// callers that already hold a resolved vault `&Path`.
pub fn ensure_vault_dir(root: &Path) -> Result<(), AppError> {
    if !root.is_dir() {
        return Err(AppError::message("vault path is not a directory"));
    }
    Ok(())
}

/// Resolve a paper folder inside the vault: sanitize `rel` (normalized
/// separators, no leading/trailing slashes), join under `vault_root`, and
/// require an existing directory. Returns `(abs_dir, sanitized_rel)`.
pub fn resolve_paper_dir(vault_root: &Path, rel: &str) -> Result<(PathBuf, String), AppError> {
    let rel = sanitize_vault_rel(rel).map_err(|_| AppError::message("invalid paper path"))?;
    let dir = vault_root.join(&rel);
    if !dir.is_dir() {
        return Err(AppError::message("paper folder not found"));
    }
    Ok((dir, rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_and_collapses() {
        assert_eq!(normalize_rel("/a//b/"), "a/b");
        assert_eq!(normalize_rel("."), "");
        assert_eq!(normalize_rel("./notes/x.md"), "notes/x.md");
        assert_eq!(normalize_rel("a/./b"), "a/b");
        assert_eq!(normalize_rel("papers\\x\\NOTES.md"), "papers/x/NOTES.md");
    }

    #[test]
    fn normalize_family_semantics_distinct() {
        // Separators-only: keeps whitespace, interior `//` and `.` segments so
        // walked/persisted paths round-trip byte-for-byte.
        assert_eq!(normalize_rel_separators("/a//b/"), "a//b");
        assert_eq!(normalize_rel_separators(" papers\\x\\ "), " papers/x/ ");
        // Lexical: no whitespace trim, `..` pops the preceding segment and a
        // leading `..` with nothing to pop is kept.
        assert_eq!(normalize_rel_lexical("a/../b"), "b");
        assert_eq!(normalize_rel_lexical("../a"), "../a");
        assert_eq!(normalize_rel_lexical(" a/./b "), " a/b ");
    }

    #[test]
    fn canonicalize_best_effort_falls_back_to_input() {
        let missing = Path::new("/agentero-no-such-dir-xyz");
        assert_eq!(canonicalize_best_effort(missing), missing.to_path_buf());
    }

    #[test]
    fn escape_detection() {
        assert!(path_escapes_root("../etc/passwd"));
        assert!(path_escapes_root("a/../../b"));
        assert!(!path_escapes_root("papers/1706.03762/NOTES.md"));
        assert!(!path_escapes_root(""));
    }

    #[test]
    fn sanitize_vault_rel_guards() {
        assert_eq!(
            sanitize_vault_rel("/papers//x\\NOTES.md").unwrap(),
            "papers/x/NOTES.md"
        );
        assert!(sanitize_vault_rel("").is_err());
        assert!(sanitize_vault_rel("a/../b").is_err());
        // Dots inside names are fine (unlike a naive contains("..") check).
        assert_eq!(sanitize_vault_rel("notes..md").unwrap(), "notes..md");
    }

    #[test]
    fn join_remote_ok() {
        assert_eq!(
            join_remote("/data/vault", "papers/x").unwrap(),
            "/data/vault/papers/x"
        );
        assert_eq!(join_remote("/data/vault/", "").unwrap(), "/data/vault");
        assert!(join_remote("/data/vault", "../x").is_err());
    }

    #[test]
    fn resolve_vault_trims_and_requires_directory() {
        let dir = std::env::temp_dir();
        let raw = dir.to_str().expect("temp dir is utf-8");
        assert_eq!(resolve_vault(raw).unwrap(), dir);
        assert_eq!(resolve_vault(&format!(" {raw} ")).unwrap(), dir);

        let missing = dir.join("agentero-no-such-vault-dir");
        let err = resolve_vault(missing.to_str().unwrap()).unwrap_err();
        assert_eq!(err.to_string(), "vault path is not a directory");
    }

    #[test]
    fn resolve_paper_dir_sanitizes_and_requires_directory() {
        let tmp =
            std::env::temp_dir().join(format!("agentero-resolve-paper-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("papers/x")).expect("create paper dir");

        let (dir, rel) = resolve_paper_dir(&tmp, " /papers//x ").expect("resolve paper dir");
        assert_eq!(rel, "papers/x");
        assert_eq!(dir, tmp.join("papers/x"));

        assert!(resolve_paper_dir(&tmp, "papers/missing").is_err());
        let escape = resolve_paper_dir(&tmp, "../escape").unwrap_err();
        assert_eq!(escape.to_string(), "invalid paper path");
        let empty = resolve_paper_dir(&tmp, " / ").unwrap_err();
        assert_eq!(empty.to_string(), "invalid paper path");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn vault_relative_normalizes_relative_and_rejects_escapes() {
        let root = Path::new("/vault");
        assert_eq!(
            vault_relative(root, "papers/a/NOTES.md").as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(
            vault_relative(root, "/vault/papers/a/NOTES.md").as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(
            vault_relative(root, "/vault/papers/./a/../a/NOTES.md").as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(vault_relative(root, "/etc/passwd"), None);
        assert_eq!(vault_relative(root, "papers/../../etc/passwd"), None);
        assert_eq!(vault_relative(root, ""), Some(String::new()));
    }

    #[test]
    fn without_verbatim_prefix_keeps_unc() {
        assert_eq!(without_verbatim_prefix(r"\\?\D:\vault"), r"D:\vault");
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\server\share"),
            r"\\?\UNC\server\share"
        );
        assert_eq!(without_verbatim_prefix("/var/vault"), "/var/vault");
    }

    #[cfg(windows)]
    #[test]
    fn vault_relative_strips_verbatim_drive_prefix() {
        assert_eq!(
            vault_relative(Path::new(r"D:\vault"), r"\\?\D:\vault\papers\a\NOTES.md").as_deref(),
            Some("papers/a/NOTES.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn vault_relative_accepts_symlink_spellings() {
        let base =
            std::env::temp_dir().join(format!("agentero-vault-rel-{}", uuid::Uuid::new_v4()));
        let real = base.join("real");
        std::fs::create_dir_all(real.join("papers/a")).expect("create paper");
        let notes = real.join("papers/a/NOTES.md");
        std::fs::write(&notes, "# A\n").expect("write notes");
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink vault");

        let through_link = link.join("papers/a/NOTES.md");
        assert_eq!(
            vault_relative(&real, through_link.to_str().expect("utf8")).as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(
            vault_relative(&link, notes.to_str().expect("utf8")).as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(
            vault_relative(&link, "papers/a/NOTES.md").as_deref(),
            Some("papers/a/NOTES.md")
        );
        assert_eq!(vault_relative(&link, "/etc/passwd"), None);

        let _ = std::fs::remove_dir_all(&base);
    }
}
