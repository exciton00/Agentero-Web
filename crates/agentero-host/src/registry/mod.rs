pub mod antigravity;
pub mod bundled;
pub mod discovery;
#[cfg(windows)]
pub(crate) mod dsh;
pub mod lifecycle;
pub mod store;
pub mod templates;
pub mod version_check;

pub use discovery::{path_entries, probe_command, resolve_command};
pub use lifecycle::{clear_lifecycle_cancel, request_lifecycle_cancel, supports_lifecycle};
pub use store::AgentRegistry;
pub use templates::{
    builtin_templates, catalog_templates, interactive_cli, template_from_id, template_info,
};
