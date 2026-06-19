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

use std::{
    ops::Range as StdRange,
    path::{Path, PathBuf},
};

use gpui::{App, AsyncApp, Entity, Task};
use language::{AutoindentMode, Buffer, BufferEditSource};
use project::Project;
use release_channel::{AppVersion, ReleaseChannel};
use workspace::MultiWorkspace;

use crate::BufferRpcServer;
use crate::positions::{
    Position, Range, decode_version, encode_version, point_utf16_to_offset, version_is_stale,
};
use crate::protocol::{
    BufferEdit, BufferEditParams, BufferEditResult, BufferInfo, BufferOpenParams, BufferOpenResult,
    BufferSaveParams, BufferSaveResult, BufferTextParams, BufferTextResult, CONFLICT,
    CURRENT_ACTIVE, ConflictErrorData, InitializeParams, InitializeResult, PROTOCOL_VERSION,
    PingResult, Request, Response, STALE_WORKSPACE, SavedMtime, WorkspaceActiveResult,
    WorkspaceInfo, WorkspaceTargetParams, same_protocol_major,
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
        "buffer/edit" => cx.update(|cx| buffer_edit(id, request.params, cx)),
        "buffer/save" => buffer_save(id, request.params, cx).await,
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
        let task = prepare_open(&project, &path, cx)?;
        Some((project, task))
    });

    let Some((project, task)) = prepared else {
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
            cx.global_mut::<BufferRpcServer>().retain_buffer(
                info.buffer_id,
                buffer.clone(),
                project,
            );
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
/// `find_or_create_worktree` (constraint 7).
///
/// NON-absolute paths (relative, or root-name-prefixed "project" paths) are
/// resolved with `Project::find_project_path`, which scans every visible
/// worktree for a matching entry — so a path under the second root resolves to
/// the second root, not blindly to the first.  We never join to the first
/// worktree root and never reimplement resolution.  An unresolved path returns
/// `None`, which the caller maps to JSON-RPC `INVALID_PARAMS`: we do not create
/// a new file via an unprefixed relative path (abs-path + `open_local_buffer`
/// already covers new-file-outside-worktree).
fn prepare_open(
    project: &Entity<Project>,
    path: &Path,
    cx: &mut App,
) -> Option<Task<anyhow::Result<Entity<Buffer>>>> {
    if path.is_absolute() {
        return Some(project.update(cx, |project, cx| project.open_local_buffer(path, cx)));
    }

    let project_path = project.read(cx).find_project_path(path, cx)?;
    Some(project.update(cx, |project, cx| project.open_buffer(project_path, cx)))
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

// ── buffer/edit ─────────────────────────────────────────────────────────────

fn buffer_edit(id: Id, params: Option<serde_json::Value>, cx: &mut App) -> Response {
    let params = match parse_params::<BufferEditParams>(params) {
        Ok(params) => params,
        Err(make_error) => return make_error(id),
    };

    if let Err(message) = validate_edit_source(params.source.as_deref()) {
        return Response::error(id, INVALID_PARAMS, message);
    }

    let Some(buffer) = find_buffer(params.buffer_id, cx) else {
        return Response::error(id, INVALID_PARAMS, "unknown bufferId");
    };

    let current_version = buffer.read(cx).version();
    if let Some(base_version) = params.base_version {
        let base_version = decode_version(base_version);
        if version_is_stale(&base_version, &current_version) {
            return conflict(id, &current_version);
        }
    }

    let text = buffer.read(cx).text_snapshot().text();
    let edits = match decode_edits(&text, params.edits) {
        Ok(edits) => edits,
        Err(message) => return Response::error(id, INVALID_PARAMS, message),
    };
    let autoindent = params
        .autoindent
        .unwrap_or(false)
        .then_some(AutoindentMode::EachLine);

    let (version, lamport) = apply_agent_edit(&buffer, edits, autoindent, cx);

    Response::result(id, BufferEditResult { version, lamport })
}

fn apply_agent_edit(
    buffer: &Entity<Buffer>,
    edits: Vec<(StdRange<usize>, String)>,
    autoindent: Option<AutoindentMode>,
    cx: &mut App,
) -> (crate::positions::WireVersion, Option<u32>) {
    buffer.update(cx, |buffer, cx| {
        buffer.finalize_last_transaction();
        buffer.start_transaction();
        let lamport = buffer
            .edit(edits, autoindent, cx)
            .map(|timestamp| timestamp.value);
        buffer.end_transaction_with_source(BufferEditSource::Agent, cx);
        (encode_version(&buffer.version()), lamport)
    })
}

fn validate_edit_source(source: Option<&str>) -> Result<(), String> {
    match source {
        None | Some("Agent") | Some("agent") => Ok(()),
        Some(source) => Err(format!(
            "unsupported edit source '{source}'; M2 supports only Agent"
        )),
    }
}

fn conflict(id: Id, current_version: &clock::Global) -> Response {
    Response::error_with_data(
        id,
        CONFLICT,
        "buffer version conflict",
        ConflictErrorData {
            kind: "Conflict",
            current_version: encode_version(current_version),
        },
    )
}

fn decode_edits(
    text: &str,
    edits: Vec<BufferEdit>,
) -> Result<Vec<(StdRange<usize>, String)>, String> {
    edits
        .into_iter()
        .map(|edit| {
            let start = offset_of(text, &edit.range.start)?;
            let end = offset_of(text, &edit.range.end)?;
            if start > end {
                return Err(format!("range start ({start}) is after end ({end})"));
            }
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                return Err("range endpoint does not fall on a UTF-8 char boundary".to_string());
            }
            Ok((start..end, edit.new_text))
        })
        .collect()
}

// ── buffer/save ─────────────────────────────────────────────────────────────

async fn buffer_save(id: Id, params: Option<serde_json::Value>, cx: &mut AsyncApp) -> Response {
    let params = match parse_params::<BufferSaveParams>(params) {
        Ok(params) => params,
        Err(make_error) => return make_error(id),
    };

    let prepared = cx.update(|cx| {
        let buffer = find_buffer(params.buffer_id, cx)?;
        let project = find_project_for_buffer(params.buffer_id, cx)?;
        let task = project.update(cx, |project, cx| project.save_buffer(buffer.clone(), cx));
        Some((buffer, task))
    });

    let Some((buffer, task)) = prepared else {
        return Response::error(id, INVALID_PARAMS, "unknown bufferId");
    };

    if let Err(error) = task.await {
        return Response::error(id, INTERNAL_ERROR, format!("buffer save failed: {error:#}"));
    }

    cx.update(|cx| {
        let buffer = buffer.read(cx);
        Response::result(
            id,
            BufferSaveResult {
                version: encode_version(&buffer.version()),
                saved_mtime: buffer
                    .saved_mtime()
                    .and_then(|mtime| mtime.to_seconds_and_nanos_for_persistence())
                    .map(|(secs_since_epoch, nanos_since_epoch)| SavedMtime {
                        secs_since_epoch,
                        nanos_since_epoch,
                    }),
            },
        )
    })
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

fn find_project_for_buffer(buffer_id: u64, cx: &mut App) -> Option<Entity<Project>> {
    if cx.has_global::<BufferRpcServer>()
        && let Some(project) = cx.global::<BufferRpcServer>().buffer_project(buffer_id)
    {
        return Some(project);
    }

    for entry in enumerate_workspaces(cx) {
        let buffers = entry.project.read(cx).opened_buffers(cx);
        if buffers
            .iter()
            .any(|buffer| buffer.read(cx).remote_id().to_proto() == buffer_id)
        {
            return Some(entry.project);
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fs::FakeFs;
    use gpui::{AppContext, TestAppContext};
    use language::{BufferEditSource, BufferEvent};
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;

    use super::*;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    /// Regression for T-04952 (daedalus #9009): in a multi-root workspace, a
    /// non-absolute path that exists under the SECOND visible worktree must
    /// resolve to the second root — NOT be blindly joined against the first
    /// root. The previous `prepare_open` joined every relative path to
    /// `first_root/<path>`, opening/creating the wrong file.
    #[gpui::test]
    async fn prepare_open_resolves_path_under_second_root(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root1"), json!({ "only1.txt": "one" }))
            .await;
        fs.insert_tree(path!("/root2"), json!({ "only2.txt": "two" }))
            .await;
        let project = Project::test(
            fs.clone(),
            [path!("/root1").as_ref(), path!("/root2").as_ref()],
            cx,
        )
        .await;

        let second_root_id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .nth(1)
                .expect("two visible worktrees")
                .read(cx)
                .id()
        });

        // A file that exists ONLY under the second root. The buggy code would
        // have joined it to /root1/only2.txt; the fix scans every worktree.
        let task = cx
            .update(|cx| prepare_open(&project, Path::new("only2.txt"), cx))
            .expect("relative path resolves to a worktree entry");
        let buffer = task.await.expect("buffer opens");

        let worktree_id = buffer.read_with(cx, |buffer, cx| {
            buffer
                .file()
                .expect("opened buffer has a file")
                .worktree_id(cx)
        });
        assert_eq!(
            worktree_id, second_root_id,
            "path under the second root must resolve to the second root, not the first"
        );
    }

    /// A non-absolute path that matches no worktree entry must NOT silently fall
    /// back to the first root / create a new file — `prepare_open` returns
    /// `None`, which the caller maps to JSON-RPC INVALID_PARAMS.
    #[gpui::test]
    async fn prepare_open_rejects_unresolved_relative_path(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root1"), json!({ "only1.txt": "one" }))
            .await;
        fs.insert_tree(path!("/root2"), json!({ "only2.txt": "two" }))
            .await;
        let project = Project::test(
            fs.clone(),
            [path!("/root1").as_ref(), path!("/root2").as_ref()],
            cx,
        )
        .await;

        let resolved =
            cx.update(|cx| prepare_open(&project, Path::new("does-not-exist.txt"), cx).is_some());
        assert!(
            !resolved,
            "an unresolved relative path must return None (→ INVALID_PARAMS), not pick the first root"
        );
    }

    #[gpui::test]
    fn agent_edit_uses_agent_source_and_one_undo_step(cx: &mut App) {
        let buffer = cx.new(|cx| Buffer::local("abcdef", cx));
        let sources = Arc::new(Mutex::new(Vec::new()));
        let _subscription = cx.subscribe(&buffer, {
            let sources = sources.clone();
            move |_, event, _| {
                if let BufferEvent::Edited { source } = event {
                    sources.lock().expect("sources lock poisoned").push(*source);
                }
            }
        });

        buffer.update(cx, |buffer, cx| {
            buffer.start_transaction();
            buffer.edit([(0..0, "user ")], None, cx);
            buffer.end_transaction(cx);
        });
        sources.lock().expect("sources lock poisoned").clear();

        let edits = vec![(5..8, "XYZ".to_string()), (11..11, "!".to_string())];
        apply_agent_edit(&buffer, edits, None, cx);

        assert_eq!(
            &*sources.lock().expect("sources lock poisoned"),
            &[BufferEditSource::Agent]
        );
        assert_eq!(buffer.read(cx).text_snapshot().text(), "user XYZdef!");

        buffer.update(cx, |buffer, cx| {
            buffer.undo(cx);
        });
        assert_eq!(buffer.read(cx).text_snapshot().text(), "user abcdef");

        buffer.update(cx, |buffer, cx| {
            buffer.undo(cx);
        });
        assert_eq!(buffer.read(cx).text_snapshot().text(), "abcdef");
    }
}
