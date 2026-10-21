//! Remote agent launch seam (SSH / local-sim).
//!
//! Desktop supported running agents inside a remote Vault over SSH. The web v1
//! explicitly drops remote Vault (`docs/development/webui.md` decision 4), so
//! this crate keeps only the **launch trait** the ACP client/probe code needs
//! to compile and to stay upstream-shaped. The remote registry/host manager
//! (`RemoteAgentHosts`, vault skill materialization) is not ported; callers in
//! the web Host always pass `None`.
//!
//! The desktop trait's `ensure_vault_skills() -> CreateVaultResult` is removed:
//! it coupled the ACP client to the desktop Vault feature.

use agentero_core::error::AppError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[async_trait]
pub trait RemoteAgentLaunch: Send + Sync {
    /// Launched over `ssh …` (vs a local-simulated remote).
    fn is_ssh(&self) -> bool;
    /// Local path that behaves like a remote (dev / tests).
    fn is_local_sim(&self) -> bool;
    /// Display host name (`user@host`).
    fn host(&self) -> &str;
    /// ACP session cwd on the remote.
    fn agent_cwd(&self) -> PathBuf;
    /// Remote workspace root (skill materialization target).
    fn work_root(&self) -> &Path;
    /// `(program, argv)` for an SSH stdio launch of `command`.
    fn ssh_stdio(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<(PathBuf, Vec<String>), AppError>;
    /// Remote `which` (probe diagnostics).
    async fn which(&self, bin: &str) -> Result<Option<String>, AppError>;
    /// Push local SKILL.md files into the remote workspace.
    async fn materialize_skills(&self) -> Result<(), AppError>;
}
