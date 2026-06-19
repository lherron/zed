use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::positions::{Range, WireVersion};

pub const JSONRPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// The literal sentinel a client may pass in the `workspace` field to target
/// the currently-focused window (resolved via `cx.active_window()`), instead of
/// pinning a concrete `workspaceId`.
pub const CURRENT_ACTIVE: &str = "current-active";

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    #[allow(dead_code)]
    pub client_name: Option<String>,
    pub protocol_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub server_version: String,
    pub protocol_version: String,
    pub pid: u32,
    pub release_channel: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    pub fn result(id: Option<Value>, result: impl Serialize) -> Self {
        let result = serde_json::to_value(result).ok();
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result,
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn error_with_data(
        id: Option<Value>,
        code: i64,
        message: impl Into<String>,
        data: impl Serialize,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
                data: serde_json::to_value(data).ok(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub fn same_protocol_major(client_version: Option<&str>) -> bool {
    let Some(client_version) = client_version else {
        return true;
    };

    client_version.split('.').next() == PROTOCOL_VERSION.split('.').next()
}

// ── M1 read-path types ──────────────────────────────────────────────────────

/// JSON-RPC error code for a stale / unknown `workspaceId` (constraint 7).
///
/// In the server-reserved range; clients match on this code (or the typed
/// `data.kind == "StaleWorkspace"`) to know the handle must be re-resolved via
/// `workspace/list` / `"current-active"`, rather than retrying blindly.
pub const STALE_WORKSPACE: i64 = -32010;

/// JSON-RPC error code for a stale `baseVersion` on `buffer/edit`.
pub const CONFLICT: i64 = -32020;

/// One entry returned by `workspace/list`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    /// Opaque, ephemeral handle (the active `Workspace`'s runtime `EntityId`,
    /// stringified). Does not survive window/workspace churn — see constraint 7.
    pub workspace_id: String,
    /// Absolute paths of the workspace's visible worktree roots.
    pub root_paths: Vec<String>,
    /// True for the currently-focused window.
    pub active: bool,
}

/// Result of `workspace/active`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceActiveResult {
    pub workspace_id: String,
}

/// Params carrying only a required `workspace` target.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceTargetParams {
    /// A concrete `workspaceId` OR the literal sentinel `"current-active"`.
    pub workspace: String,
}

/// Params for `buffer/open`.
#[derive(Debug, Clone, Deserialize)]
pub struct BufferOpenParams {
    /// A concrete `workspaceId` OR the literal sentinel `"current-active"`.
    pub workspace: String,
    /// Absolute filesystem path, or a path relative to one of the workspace's
    /// visible worktree roots.
    pub path: String,
}

/// One entry returned by `buffer/list`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferInfo {
    pub buffer_id: u64,
    pub path: Option<String>,
    pub worktree_id: Option<u64>,
    pub dirty: bool,
    pub version: WireVersion,
}

/// Result of `buffer/open`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferOpenResult {
    pub buffer_id: u64,
    pub path: Option<String>,
    pub version: WireVersion,
}

/// Params for `buffer/text`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferTextParams {
    pub buffer_id: u64,
    #[serde(default)]
    pub range: Option<Range>,
}

/// Result of `buffer/text`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferTextResult {
    pub text: String,
    pub version: WireVersion,
}

/// One edit inside `buffer/edit`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferEdit {
    pub range: Range,
    pub new_text: String,
}

/// Params for `buffer/edit`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferEditParams {
    pub buffer_id: u64,
    pub edits: Vec<BufferEdit>,
    #[serde(default)]
    pub base_version: Option<WireVersion>,
    #[serde(default)]
    pub autoindent: Option<bool>,
    #[serde(default)]
    pub source: Option<String>,
}

/// Result of `buffer/edit`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferEditResult {
    pub version: WireVersion,
    pub lamport: Option<u32>,
}

/// Params for `buffer/save`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferSaveParams {
    pub buffer_id: u64,
}

/// Result of `buffer/save`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferSaveResult {
    pub version: WireVersion,
    pub saved_mtime: Option<SavedMtime>,
}

/// Filesystem mtime persisted as Unix seconds + nanoseconds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SavedMtime {
    pub secs_since_epoch: u64,
    pub nanos_since_epoch: u32,
}

/// Typed JSON-RPC error data for `buffer/edit` base-version conflicts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictErrorData {
    pub kind: &'static str,
    pub current_version: WireVersion,
}
