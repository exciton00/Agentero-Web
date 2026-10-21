//! Agent-specific command discovery helpers.
//!
//! Generic PATH / executable resolution lives in `crate::core::process::discover`;
//! this module re-exports the surface used by the Agent registry.

pub use crate::core::process::discover::{
    login_shell_env, path_entries, probe_command, resolve_command,
};

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn finds_sh_on_unix() {
        let p = resolve_command("sh");
        assert!(p.is_some());
    }

    #[test]
    fn probe_command_errors_for_missing_bin() {
        let err = super::probe_command("__agentero_missing_binary_xyz__").unwrap_err();
        assert!(err.contains("not found"));
    }
}
