//! Method dispatch for the Buffer RPC server.
//!
//! Threading (constraint 1): this module runs on the **foreground** drain task
//! (`lib.rs`).  Entity access happens only via `AsyncApp::update` (i.e. on the
//! app thread); async ops (`open_local_buffer` returns a `Task`) are awaited in
//! the spawned future.  No socket I/O or JSON framing happens here — the writer
//! half of the transport owns all wire encoding.
//!
//! **RPC-opened buffers are invisible and carry NO language-server guarantee**
//! (constraint 6): `open_local_buffer` creates an invisible worktree and does
//! not register the buffer with any LSP.  M1 intentionally does not wire LSP.

use std::path::{Path, PathBuf};

use gpui::{App, AsyncApp, Entity, Task};
use language::Buffer;
use project::Project;
use release_channel::{AppVersion, ReleaseChannel};
use workspace::MultiWorkspace;

use crate::BufferRpcServer;
use crate::positions::{Position, Range, encode_version, point_utf16_to_offset};
use crate::protocol::{
    BufferInfo, BufferOpenParams, BufferOpenResult, BufferTextParams, BufferTextResult,
    CURRENT_ACTIVE, InitializeParams, InitializeResult, PROTOCOL_VERSION, PingResult, Request,
    Response, STALE_WORKSPACE, WorkspaceActiveResult, WorkspaceInfo, WorkspaceTargetParams,
    same_protocol_major,
};

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

type Id = Option<serde_json::Value>;

/// Dispatch a single parsed request to its handler.
///
/// Runs on the foreground drain task; `cx` is the app's async handle.  Sync
/// entity reads use `cx.update(...)`; async buffer opens are awaited here.
pub async fn dispatch(request: Request, cx: &mut AsyncApp) -> Option<Response> {
    if request.jsonrpc != crate::protocol::JSONRPC_VERSION {
        return Some(Response::error(
            request.id,
            INVALID_REQUEST,
            "invalid JSON-RPC version",
        ));
    }

    let id = request.id.clone();
    let response = match request.method.as_str() {
        "initialize" => cx.update(|cx| initialize(id, request.params, cx)),
        "ping" => Response::result(id, PingResult { ok: true }),
        "workspace/list" => cx.update(|cx| workspace_list(id, cx)),
        "workspace/active" => cx.update(|cx| workspace_active(id, cx)),
        "buffer/list" => cx.update(|cx| buffer_list(id, request.params, cx)),
        "buffer/open" => buffer_open(id, request.params, cx).await,
        "buffer/text" => cx.update(|cx| buffer_text(id, request.params, cx)),
        _ => Response::error(id, METHOD_NOT_FOUND, "method not found"),
    };
    Some(response)
}

fn initialize(id: Id, params: Option<serde_json::Value>, cx: &mut App) -> Response {
    let params = match params {
        Some(params) => match serde_json::from_value::<InitializeParams>(params) {
            Ok(params) => params,
            Err(_) => return Response::error(id, INVALID_PARAMS, "invalid initialize params"),
        },
        None => InitializeParams {
            client_name: None,
            protocol_version: None,
        },
    };

    if !same_protocol_major(params.protocol_version.as_deref()) {
        return Response::error(id, INVALID_PARAMS, "unsupported protocol major version");
    }

    let result = InitializeResult {
        server_version: AppVersion::global(cx).to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        pid: std::process::id(),
        release_channel: ReleaseChannel::try_global(cx)
            .unwrap_or_default()
            .dev_name()
            .to_string(),
    };

    Response::result(id, result)
}

// ── Workspace enumeration / resolution ──────────────────────────────────────

/// A window's active workspace: its window id, the workspace's `EntityId`
/// (stringified into the opaque `workspaceId`), and the backing project.
struct WorkspaceEntry {
    window_id: u64,
    workspace_id: u64,
    project: Entity<Project>,
}

/// Enumerate every open window that hosts a `MultiWorkspace`, resolving each to
/// its currently-active workspace + project.
fn enumerate_workspaces(cx: &mut App) -> Vec<WorkspaceEntry> {
    let mut entries = Vec::new();
    for window in cx.windows() {
        let Some(handle) = window.downcast::<MultiWorkspace>() else {
            continue;
        };
        let Ok(multi) = handle.read(cx) else {
            continue;
        };
        let workspace = multi.workspace().clone();
        let workspace_id = workspace.entity_id().as_u64();
        let project = workspace.read(cx).project().clone();
        entries.push(WorkspaceEntry {
            window_id: window.window_id().as_u64(),
            workspace_id,
            project,
        });
    }
    entries
}

/// Resolve the `workspace` target (a concrete `workspaceId` or the
/// `"current-active"` sentinel) to its project.
///
/// Returns `None` for a stale / unknown id or when no window is focused —
/// callers turn that into a typed `StaleWorkspace` error (constraint 7: never a
/// panic, never a silent fallback to another window).
fn resolve_project(selector: &str, cx: &mut App) -> Option<Entity<Project>> {
    if selector == CURRENT_ACTIVE {
        let window = cx.active_window()?;
        let handle = window.downcast::<MultiWorkspace>()?;
        let multi = handle.read(cx).ok()?;
        let workspace = multi.workspace().clone();
        return Some(workspace.read(cx).project().clone());
    }

    let target: u64 = selector.parse().ok()?;
    enumerate_workspaces(cx)
        .into_iter()
        .find(|entry| entry.workspace_id == target)
        .map(|entry| entry.project)
}

fn stale_workspace(id: Id, selector: &str) -> Response {
    Response::error(
        id,
        STALE_WORKSPACE,
        format!("workspace '{selector}' is stale or unknown; re-list or use 'current-active'"),
    )
}

// ── workspace/list ──────────────────────────────────────────────────────────

fn workspace_list(id: Id, cx: &mut App) -> Response {
    let active_window = cx.active_window().map(|window| window.window_id().as_u64());
    let entries = enumerate_workspaces(cx);
    let infos: Vec<WorkspaceInfo> = entries
        .iter()
        .map(|entry| {
            let root_paths = entry
                .project
                .read(cx)
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
                .collect();
            WorkspaceInfo {
                workspace_id: entry.workspace_id.to_string(),
                root_paths,
                active: Some(entry.window_id) == active_window,
            }
        })
        .collect();
    Response::result(id, infos)
}

// ── workspace/active ────────────────────────────────────────────────────────

fn workspace_active(id: Id, cx: &mut App) -> Response {
    let Some(window) = cx.active_window() else {
        return Response::error(id, INTERNAL_ERROR, "no active window");
    };
    let Some(handle) = window.downcast::<MultiWorkspace>() else {
        return Response::error(id, INTERNAL_ERROR, "active window is not a workspace");
    };
    let Ok(multi) = handle.read(cx) else {
        return Response::error(id, INTERNAL_ERROR, "active workspace unavailable");
    };
    let workspace_id = multi.workspace().entity_id().as_u64();
    Response::result(
        id,
        WorkspaceActiveResult {
            workspace_id: workspace_id.to_string(),
        },
    )
}

// ── buffer/list ─────────────────────────────────────────────────────────────

fn buffer_list(id: Id, params: Option<serde_json::Value>, cx: &mut App) -> Response {
    let params = match parse_params::<WorkspaceTargetParams>(params) {
        Ok(params) => params,
        Err(make_error) => return make_error(id),
    };

    let Some(project) = resolve_project(&params.workspace, cx) else {
        return stale_workspace(id, &params.workspace);
    };

    let buffers = project.read(cx).opened_buffers(cx);
    let infos: Vec<BufferInfo> = buffers
        .iter()
        .map(|buffer| buffer_info(buffer, cx))
        .collect();
    Response::result(id, infos)
}

fn buffer_info(buffer: &Entity<Buffer>, cx: &App) -> BufferInfo {
    let buffer = buffer.read(cx);
    let file = buffer.file();
    BufferInfo {
        buffer_id: buffer.remote_id().to_proto(),
        path: file.map(|file| file.full_path(cx).to_string_lossy().into_owned()),
        worktree_id: file.map(|file| file.worktree_id(cx).to_proto()),
        dirty: buffer.is_dirty(),
        version: encode_version(&buffer.version()),
    }
}

// ── buffer/open ─────────────────────────────────────────────────────────────

async fn buffer_open(id: Id, params: Option<serde_json::Value>, cx: &mut AsyncApp) -> Response {
    let params = match parse_params::<BufferOpenParams>(params) {
        Ok(params) => params,
        Err(make_error) => return make_error(id),
    };

    let path = PathBuf::from(&params.path);
    // Resolve the workspace and build the (async) open task on the foreground.
    let prepared = cx.update(|cx| {
        let project = resolve_project(&params.workspace, cx)?;
        prepare_open(&project, &path, cx)
    });

    let Some(task) = prepared else {
        // Either a stale workspace or an unresolvable relative path.  Re-resolve
        // once to distinguish for a precise error.
        let resolvable = cx.update(|cx| resolve_project(&params.workspace, cx).is_some());
        return if resolvable {
            Response::error(
                id,
                INVALID_PARAMS,
                "could not resolve path against any worktree",
            )
        } else {
            stale_workspace(id, &params.workspace)
        };
    };

    // Await the open off the synchronous update (constraint 1).
    let buffer = match task.await {
        Ok(buffer) => buffer,
        Err(error) => {
            return Response::error(id, INTERNAL_ERROR, format!("buffer open failed: {error:#}"));
        }
    };

    cx.update(|cx| {
        let info = buffer_info(&buffer, cx);
        // Retain a strong handle: BufferStore holds only a weak ref, and an
        // RPC-opened invisible buffer has no editor keeping it alive, so without
        // this it would be dropped before the next buffer/text call.
        if cx.has_global::<BufferRpcServer>() {
            cx.global_mut::<BufferRpcServer>()
                .retain_buffer(info.buffer_id, buffer.clone());
        }
        Response::result(
            id,
            BufferOpenResult {
                buffer_id: info.buffer_id,
                path: info.path,
                version: info.version,
            },
        )
    })
}

/// Build the open `Task` for `path`.
///
/// Absolute paths go through `Project::open_local_buffer`, which creates the
/// invisible worktree itself for paths outside all existing worktrees (and
/// reuses an existing worktree otherwise) — we do not re-implement
/// `find_or_create_worktree` (constraint 7).  A relative path is joined against
/// the first visible worktree root, then opened the same way.
fn prepare_open(
    project: &Entity<Project>,
    path: &Path,
    cx: &mut App,
) -> Option<Task<anyhow::Result<Entity<Buffer>>>> {
    if path.is_absolute() {
        return Some(project.update(cx, |project, cx| project.open_local_buffer(path, cx)));
    }

    let root = project
        .read(cx)
        .visible_worktrees(cx)
        .next()?
        .read(cx)
        .abs_path();
    let abs_path = root.join(path);
    Some(project.update(cx, |project, cx| project.open_local_buffer(&abs_path, cx)))
}

// ── buffer/text ─────────────────────────────────────────────────────────────

fn buffer_text(id: Id, params: Option<serde_json::Value>, cx: &mut App) -> Response {
    let params = match parse_params::<BufferTextParams>(params) {
        Ok(params) => params,
        Err(make_error) => return make_error(id),
    };

    let Some(buffer) = find_buffer(params.buffer_id, cx) else {
        return Response::error(id, INVALID_PARAMS, "unknown bufferId");
    };

    let snapshot = buffer.read(cx).text_snapshot();
    let full = snapshot.text();
    let version = encode_version(snapshot.version());

    let text = match params.range {
        None => full,
        Some(range) => match slice_range(&full, &range) {
            Ok(text) => text,
            Err(message) => return Response::error(id, INVALID_PARAMS, message),
        },
    };

    Response::result(id, BufferTextResult { text, version })
}

/// Find an opened buffer by its protocol id.
///
/// Checks the server's retained RPC-opened buffers first (invisible buffers not
/// reachable via any project), then every window's project buffer store.
fn find_buffer(buffer_id: u64, cx: &mut App) -> Option<Entity<Buffer>> {
    if cx.has_global::<BufferRpcServer>()
        && let Some(buffer) = cx.global::<BufferRpcServer>().buffer(buffer_id)
    {
        return Some(buffer);
    }

    for entry in enumerate_workspaces(cx) {
        let buffers = entry.project.read(cx).opened_buffers(cx);
        if let Some(buffer) = buffers
            .into_iter()
            .find(|buffer| buffer.read(cx).remote_id().to_proto() == buffer_id)
        {
            return Some(buffer);
        }
    }
    None
}

/// Slice `text` to the byte range described by `range`.
///
/// Each endpoint uses its `offset` byte form when present, else converts the
/// UTF-16 `{line, character}` to a byte offset.  Returns a human-readable error
/// (never panics) for out-of-range, inverted, or non-char-boundary offsets.
fn slice_range(text: &str, range: &Range) -> Result<String, String> {
    let start = offset_of(text, &range.start)?;
    let end = offset_of(text, &range.end)?;
    if start > end {
        return Err(format!("range start ({start}) is after end ({end})"));
    }
    if end > text.len() {
        return Err(format!(
            "range end ({end}) exceeds buffer length ({})",
            text.len()
        ));
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err("range endpoint does not fall on a UTF-8 char boundary".to_string());
    }
    Ok(text[start..end].to_string())
}

fn offset_of(text: &str, position: &Position) -> Result<usize, String> {
    if let Some(offset) = position.offset {
        if offset > text.len() {
            return Err(format!(
                "byte offset {offset} exceeds buffer length ({})",
                text.len()
            ));
        }
        return Ok(offset);
    }
    point_utf16_to_offset(text, position.line, position.character)
        .map_err(|error| error.to_string())
}

// ── Param parsing helper ────────────────────────────────────────────────────

/// Parse `params` into `T`, or return a closure that builds an `INVALID_PARAMS`
/// error response for the request id.
fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, impl FnOnce(Id) -> Response> {
    let value = params.unwrap_or(serde_json::Value::Null);
    serde_json::from_value::<T>(value).map_err(|error| {
        move |id: Id| Response::error(id, INVALID_PARAMS, format!("invalid params: {error}"))
    })
}
