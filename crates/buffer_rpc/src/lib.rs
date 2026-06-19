//! Buffer RPC server — in-process UDS bridge for Zed.
//!
//! This crate embeds a small JSON-RPC 2.0 server inside a Zed fork, listening
//! on a Unix domain socket.  External local processes can enumerate, read, edit,
//! save, and watch the live in-memory buffers without reimplementing the CRDT.
//!
//! See `docs/buffer-rpc-proposal.md` for the full design and daedalus-approved
//! constraints.
//!
//! # Module layout
//! - [`framing`] — LSP-style `Content-Length` encoder/decoder (GPUI-free, byte logic).
//!   This module is the RED-TEST surface for M0; bodies are stubs (`todo!()`).
//! - [`transport`] — UDS accept loop, per-connection reader/writer halves, conn registry.
//! - [`protocol`] — serde JSON-RPC 2.0 types, schema source of truth.
//! - [`handlers`] — M0 `initialize` and `ping` dispatch.
//! - [`positions`] — stub for future UTF-16/anchor codecs.
//!
//! # Off-by-default
//! The server is disabled unless `ZED_BUFFER_RPC_SOCK` is set or the settings flag is on.
//! `socket_path_from_env_or_settings` returns `None` in that case; the `app.run` call site
//! in `main.rs` uses it as a gate.

use std::{collections::HashMap, env, path::PathBuf};

use futures::StreamExt;
use gpui::{App, Entity, Global};
use language::Buffer;

pub mod framing;
pub mod handlers;
pub mod notify;
pub mod positions;
pub mod protocol;
pub mod transport;

pub struct BufferRpcServer {
    socket_path: PathBuf,
    /// Strong handles to buffers opened via `buffer/open`.
    ///
    /// `BufferStore` only holds `WeakEntity<Buffer>` and relies on the opener to
    /// retain a strong reference; an RPC-opened *invisible* buffer has no editor
    /// keeping it alive, so the server must retain it here to keep it reachable
    /// by `buffer/text` (and later `buffer/edit`/`buffer/save`).  Keyed by the
    /// protocol buffer id (`BufferId::to_proto`).
    opened_buffers: HashMap<u64, OpenedBuffer>,
}

struct OpenedBuffer {
    buffer: Entity<Buffer>,
    project: Entity<project::Project>,
}

impl Global for BufferRpcServer {}

impl BufferRpcServer {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Retain a strong handle to an RPC-opened buffer so it stays alive.
    pub fn retain_buffer(
        &mut self,
        buffer_id: u64,
        buffer: Entity<Buffer>,
        project: Entity<project::Project>,
    ) {
        self.opened_buffers
            .insert(buffer_id, OpenedBuffer { buffer, project });
    }

    /// Look up a previously RPC-opened buffer by its protocol id.
    pub fn buffer(&self, buffer_id: u64) -> Option<Entity<Buffer>> {
        self.opened_buffers
            .get(&buffer_id)
            .map(|opened| opened.buffer.clone())
    }

    /// Look up the project that opened a previously RPC-opened buffer.
    pub fn buffer_project(&self, buffer_id: u64) -> Option<Entity<project::Project>> {
        self.opened_buffers
            .get(&buffer_id)
            .map(|opened| opened.project.clone())
    }
}

pub fn init(path: PathBuf, cx: &mut App) {
    let mut requests = match transport::start(path.clone()) {
        Ok(requests) => requests,
        Err(error) => {
            eprintln!(
                "failed to start Buffer RPC listener at {}: {error:#}",
                path.display()
            );
            return;
        }
    };

    cx.set_global(BufferRpcServer {
        socket_path: path,
        opened_buffers: HashMap::new(),
    });
    cx.spawn(async move |cx| {
        // Foreground drain task (constraint 1): entity access happens via
        // `cx.update` inside `handlers::dispatch`; async buffer opens are awaited
        // here in the spawned future.  No socket I/O or JSON framing on this
        // thread — the transport's writer half owns all wire encoding.
        while let Some(request) = requests.next().await {
            let response = handlers::dispatch(request.request, cx).await;
            request.responder.respond(response);
        }
    })
    .detach();
}

pub fn socket_path_from_env_or_settings(_cx: &App) -> Option<PathBuf> {
    let path = env::var_os("ZED_BUFFER_RPC_SOCK")?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
