use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::positions::{Position, Range, WireVersion};

pub const JSONRPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// The literal sentinel a client may pass in the `workspace` field to target
/// the currently-focused window (resolved via `cx.active_window()`), instead of
/// pinning a concrete `workspaceId`.
pub const CURRENT_ACTIVE: &str = "current-active";

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct Request {
    pub jsonrpc: String,
    #[ts(optional = nullable)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InitializeParams {
    #[allow(dead_code)]
    #[ts(optional = nullable)]
    pub client_name: Option<String>,
    #[ts(optional = nullable)]
    pub protocol_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InitializeResult {
    pub server_version: String,
    pub protocol_version: String,
    pub pid: u32,
    pub release_channel: String,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PingResult {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
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

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ResponseError {
    #[ts(type = "number")]
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
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
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
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
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorkspaceActiveResult {
    pub workspace_id: String,
}

/// Params carrying only a required `workspace` target.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct WorkspaceTargetParams {
    /// A concrete `workspaceId` OR the literal sentinel `"current-active"`.
    pub workspace: String,
}

/// Params for `buffer/open`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct BufferOpenParams {
    /// A concrete `workspaceId` OR the literal sentinel `"current-active"`.
    pub workspace: String,
    /// Absolute filesystem path, or a path relative to one of the workspace's
    /// visible worktree roots.
    pub path: String,
}

/// One entry returned by `buffer/list`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferInfo {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub path: Option<String>,
    #[ts(as = "Option<u32>")]
    pub worktree_id: Option<u64>,
    pub dirty: bool,
    pub version: WireVersion,
}

/// Result of `buffer/open`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferOpenResult {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub path: Option<String>,
    pub version: WireVersion,
}

/// Params for `buffer/text`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferTextParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub range: Option<Range>,
}

/// Result of `buffer/text`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferTextResult {
    pub text: String,
    pub version: WireVersion,
}

/// One edit inside `buffer/edit`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferEdit {
    pub range: Range,
    pub new_text: String,
}

/// Params for `buffer/edit`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferEditParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub edits: Vec<BufferEdit>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub base_version: Option<WireVersion>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub autoindent: Option<bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub source: Option<String>,
}

/// Result of `buffer/edit`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferEditResult {
    pub version: WireVersion,
    pub lamport: Option<u32>,
}

/// Params for `buffer/save`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferSaveParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
}

/// Result of `buffer/save`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferSaveResult {
    pub version: WireVersion,
    pub saved_mtime: Option<SavedMtime>,
}

/// Filesystem mtime persisted as Unix seconds + nanoseconds.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct SavedMtime {
    #[ts(type = "number")]
    pub secs_since_epoch: u64,
    pub nanos_since_epoch: u32,
}

/// Typed JSON-RPC error data for `buffer/edit` base-version conflicts.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConflictErrorData {
    pub kind: &'static str,
    pub current_version: WireVersion,
}

// ── M3 notification / subscription / anchor types ───────────────────────────

use crate::notify::{WireAnchorToken, WireChange};

/// A server→client JSON-RPC 2.0 notification (no `id`).
///
/// Emitted on a subscribed connection's bounded outbound channel for
/// `buffer/didChange`, `buffer/didSave`, `buffer/didOpen`, `buffer/didClose`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: impl Serialize) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method: method.into(),
            params: serde_json::to_value(params).unwrap_or(Value::Null),
        }
    }
}

/// A message the transport's writer half serialises to the socket: either a
/// reply to a request or a server-initiated notification. Serialised untagged
/// so each variant emits its own JSON-RPC shape.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outbound {
    Response(Response),
    Notification(Notification),
}

pub const METHOD_DID_CHANGE: &str = "buffer/didChange";
pub const METHOD_DID_SAVE: &str = "buffer/didSave";
pub const METHOD_DID_OPEN: &str = "buffer/didOpen";
pub const METHOD_DID_CLOSE: &str = "buffer/didClose";

/// Params for `buffer/subscribe`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferSubscribeParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
}

/// Result of `buffer/subscribe`.
///
/// Establishes the **baseline** the subscriber is expected to hold: `text` is
/// the buffer contents at `version`, captured atomically with the subscription
/// registration. The client stores `text` as its current document and the
/// server initialises that subscriber's delivery cursor to `version`; every
/// subsequent `buffer/didChange` diffs forward from there (ranges in the
/// client's held frame). No edit can interleave between this snapshot and the
/// first delivered notification.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferSubscribeResult {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub version: WireVersion,
    pub text: String,
}

/// Params for `buffer/unsubscribe`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BufferUnsubscribeParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
}

/// Result of `buffer/unsubscribe`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BufferUnsubscribeResult {
    pub ok: bool,
}

/// Anchor bias on the wire (`Left` ≈ `anchor_before`, `Right` ≈ `anchor_after`).
#[derive(Debug, Clone, Copy, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum WireBias {
    Left,
    Right,
}

/// Params for `anchor/create`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnchorCreateParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub position: Position,
    /// Defaults to `Right` (stick after inserted text) when absent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub bias: Option<WireBias>,
}

/// Result of `anchor/create`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AnchorCreateResult {
    pub anchor: WireAnchorToken,
}

/// Params for `anchor/resolve`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnchorResolveParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub anchor: WireAnchorToken,
}

/// Result of `anchor/resolve`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AnchorResolveResult {
    pub position: Position,
    pub valid: bool,
}

/// `buffer/didChange` notification params (constraint 4 — pre-edit frame).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DidChangeParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub version: WireVersion,
    /// Changes in the subscriber's held (pre-edit) frame, ordered descending by
    /// start so naive in-order application reconstructs the new text exactly.
    pub changes: Vec<WireChange>,
    pub is_local: bool,
}

/// `buffer/didSave` notification params.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DidSaveParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    pub version: WireVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub saved_mtime: Option<SavedMtime>,
}

/// `buffer/didOpen` notification params.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DidOpenParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub path: Option<String>,
}

/// `buffer/didClose` notification params.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DidCloseParams {
    #[ts(type = "number")]
    pub buffer_id: u64,
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use crate::positions::{Position, Range, WireVersion};
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    use ts_rs::{Config, TS};

    fn export<T: TS + 'static>(config: &Config) -> anyhow::Result<()> {
        T::export_all(config)?;
        Ok(())
    }

    fn strip_trailing_whitespace(path: &Path) -> anyhow::Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                strip_trailing_whitespace(&path)?;
            } else if path.extension().is_some_and(|extension| extension == "ts") {
                let contents = fs::read_to_string(&path)?;
                let mut normalized = String::new();
                for line in contents.lines() {
                    normalized.push_str(line.trim_end());
                    normalized.push('\n');
                }
                if normalized != contents {
                    fs::write(&path, normalized)?;
                }
            }
        }

        Ok(())
    }

    #[test]
    fn export_bindings() -> anyhow::Result<()> {
        let bindings_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bindings");
        fs::create_dir_all(&bindings_dir)?;

        let config = Config::default()
            .with_large_int("number")
            .with_out_dir(&bindings_dir);

        export::<Position>(&config)?;
        export::<Range>(&config)?;
        export::<WireVersion>(&config)?;
        export::<Request>(&config)?;
        export::<InitializeParams>(&config)?;
        export::<InitializeResult>(&config)?;
        export::<PingResult>(&config)?;
        export::<Response>(&config)?;
        export::<ResponseError>(&config)?;
        export::<WorkspaceInfo>(&config)?;
        export::<WorkspaceActiveResult>(&config)?;
        export::<WorkspaceTargetParams>(&config)?;
        export::<BufferOpenParams>(&config)?;
        export::<BufferInfo>(&config)?;
        export::<BufferOpenResult>(&config)?;
        export::<BufferTextParams>(&config)?;
        export::<BufferTextResult>(&config)?;
        export::<BufferEdit>(&config)?;
        export::<BufferEditParams>(&config)?;
        export::<BufferEditResult>(&config)?;
        export::<BufferSaveParams>(&config)?;
        export::<BufferSaveResult>(&config)?;
        export::<SavedMtime>(&config)?;
        export::<ConflictErrorData>(&config)?;

        // M3 notification / subscription / anchor surface.
        export::<crate::notify::WireChange>(&config)?;
        export::<crate::notify::WireAnchorToken>(&config)?;
        export::<Notification>(&config)?;
        export::<BufferSubscribeParams>(&config)?;
        export::<BufferSubscribeResult>(&config)?;
        export::<BufferUnsubscribeParams>(&config)?;
        export::<BufferUnsubscribeResult>(&config)?;
        export::<WireBias>(&config)?;
        export::<AnchorCreateParams>(&config)?;
        export::<AnchorCreateResult>(&config)?;
        export::<AnchorResolveParams>(&config)?;
        export::<AnchorResolveResult>(&config)?;
        export::<DidChangeParams>(&config)?;
        export::<DidSaveParams>(&config)?;
        export::<DidOpenParams>(&config)?;
        export::<DidCloseParams>(&config)?;

        strip_trailing_whitespace(&bindings_dir)?;

        Ok(())
    }
}
