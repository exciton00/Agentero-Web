//! Remote PDF layout analysis: shared wire types, the progress event, and
//! the provider registry. Engine implementations live in per-provider files
//! (`paddle.rs`, `mineru.rs`); commands dispatch through [`engine::engine_for`].
//!
//! Tauri-free since the headless Web port: the host seam is
//! [`crate::app_handle::AppHandle`] (desktop wraps its emitter, the Web host
//! routes into the WebSocket event bus, the CLI no-ops). Desktop command
//! shells live in `src-tauri/.../layout/hosted/commands.rs`.
//!
//! Engines return raw layout detection boxes; coordinate conversion stays in
//! the frontend.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_handle::AppHandle;
use crate::features::system::settings::{is_translate_api_key_mask, AppSettingsStore};

pub mod engine;
pub mod mineru;
pub mod openai_compatible;
pub mod paddle;

// Re-export the engine facade so hosts (desktop command shell, Web RPC) only
// need this one module path.
pub use engine::{
    engine_for, HostedLayoutAnalyzer, HostedProviderCredentials, LayoutAnalyzeContext,
};

/// Default remote provider id when a command omits `provider`.
pub const DEFAULT_REMOTE_PROVIDER: &str = "paddle";

/// Cloud-job progress callback: `(phase, extracted_pages, total_pages)`.
pub(crate) type ProgressFn<'a> = &'a (dyn Fn(&str, Option<u64>, Option<u64>) + Send + Sync);
/// Cooperative-cancel check polled between cloud-job phases.
pub(crate) type CancelFn<'a> = &'a (dyn Fn() -> bool + Send + Sync);

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemoteBox {
    pub cls_id: i64,
    pub label: String,
    pub score: f64,
    /// `[x1, y1, x2, y2]` in rendered-image pixels (top-left origin).
    pub coordinate: [f64; 4],
    /// OCR/content text for this box, when provided by the engine.
    #[specta(optional)]
    pub text: Option<String>,
    /// Caption text attached to a host without independent caption geometry.
    #[specta(optional)]
    pub caption: Option<String>,
}

/// Map raw `layout_det_res.boxes` entries into the API shape.
pub fn parse_det_boxes(boxes: &[Value]) -> Vec<LayoutRemoteBox> {
    let mut out = Vec::with_capacity(boxes.len());
    for b in boxes {
        let Some(coordinate) = b.get("coordinate").and_then(Value::as_array) else {
            continue;
        };
        if coordinate.len() != 4 {
            continue;
        }
        let nums: Vec<f64> = coordinate.iter().filter_map(Value::as_f64).collect();
        if nums.len() != 4 {
            continue;
        }
        out.push(LayoutRemoteBox {
            cls_id: b.get("cls_id").and_then(Value::as_i64).unwrap_or(-1),
            label: b
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            score: b.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            coordinate: [nums[0], nums[1], nums[2], nums[3]],
            text: None,
            caption: None,
        });
    }
    out
}

/// Progress event consumed by the layout runner for the progress bar.
pub const CLOUD_PROGRESS_EVENT: &str = "layout-remote:progress";

#[derive(Debug, Clone, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemoteAnalyzePdfArgs {
    /// Base64-encoded PDF bytes.
    pub pdf_base64: String,
    /// Upload file name (default `paper.pdf`).
    #[serde(default)]
    pub file_name: Option<String>,
    /// Access-token override (probe flow); normally resolved from settings.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Correlates `layout-remote:progress` when several API jobs run at once.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Remote provider id (defaults to `paddle`).
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemotePageResult {
    pub boxes: Vec<LayoutRemoteBox>,
    /// Page size in the same units as box coordinates (paddle: rendered px,
    /// mineru: PDF points); null makes the frontend assume a 144 DPI render.
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemoteAnalyzePdfResult {
    pub pages: Vec<LayoutRemotePageResult>,
    /// Per-page rendered pixel sizes reported by the service
    /// (`dataInfo.pages[].width/height`); empty when absent.
    pub rendered_pages: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudProgressPayload {
    phase: String,
    extracted_pages: Option<u64>,
    total_pages: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

/// Emit one `layout-remote:progress` event (payload shared by all engines).
pub(crate) fn emit_cloud_progress(
    app: &AppHandle,
    phase: &str,
    extracted_pages: Option<u64>,
    total_pages: Option<u64>,
    request_id: Option<String>,
) {
    app.emit(
        CLOUD_PROGRESS_EVENT,
        &CloudProgressPayload {
            phase: phase.to_string(),
            extracted_pages,
            total_pages,
            request_id,
        },
    );
}

/// Resolve provider credentials from the Host-held settings and inject the
/// stored access token into `api_key` (renderers only ever send a `*` mask or
/// nothing at all).
///
/// Shared by the desktop command shell and the Web RPC handler so credential
/// lookup, mask handling and defaulting cannot drift between hosts.
pub fn resolve_remote_credentials(
    store: &AppSettingsStore,
    provider: &str,
    api_key: &mut Option<String>,
) -> HostedProviderCredentials {
    let needs_stored_key = api_key
        .as_deref()
        .map(|key| {
            let trimmed = key.trim();
            trimmed.is_empty() || is_translate_api_key_mask(trimmed)
        })
        .unwrap_or(true);
    if needs_stored_key {
        if let Some(key) = store.layout_api_key(provider) {
            *api_key = Some(key);
        } else if api_key.as_deref().is_some_and(is_translate_api_key_mask) {
            *api_key = None;
        }
    }
    HostedProviderCredentials {
        api_key: api_key.take(),
        base_url: store.layout_base_url(provider),
        language: store.layout_language(provider),
        is_ocr: store.layout_is_ocr(provider),
        self_hosted: store.layout_self_hosted(provider),
    }
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemoteProbeArgs {
    /// Base64-encoded tiny probe image (JPEG).
    pub image_base64: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Remote provider id (defaults to `paddle`).
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRemoteProbeResult {
    pub job_id: String,
}

/// Wire-shape guard for the `layout-remote:progress` payload consumed by
/// `bindings.ts` (`LayoutRemoteProgressEvent`): camelCase keys, both
/// `requestId` variants (the `None` one must be omitted).
#[cfg(test)]
mod progress_payload_shape_tests {
    use super::{CloudProgressPayload, CLOUD_PROGRESS_EVENT};

    #[test]
    fn event_name_stays_stable() {
        assert_eq!(CLOUD_PROGRESS_EVENT, "layout-remote:progress");
    }

    #[test]
    fn progress_payload_matches_bindings_shape() {
        let payload = CloudProgressPayload {
            phase: "extract".to_string(),
            extracted_pages: Some(3),
            total_pages: Some(10),
            request_id: Some("req-1".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            serde_json::json!({
                "phase": "extract",
                "extractedPages": 3,
                "totalPages": 10,
                "requestId": "req-1",
            })
        );
        let without_request = CloudProgressPayload {
            phase: "uploading".to_string(),
            extracted_pages: None,
            total_pages: None,
            request_id: None,
        };
        let value = serde_json::to_value(&without_request).unwrap();
        assert_eq!(value["phase"], "uploading");
        assert!(value.get("requestId").is_none());
    }
}
