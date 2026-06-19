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

use std::{env, path::PathBuf};

use futures::StreamExt;
use gpui::{App, Global};

pub mod framing;
pub mod handlers;
pub mod positions;
pub mod protocol;
pub mod transport;

pub struct BufferRpcServer {
    socket_path: PathBuf,
}

impl Global for BufferRpcServer {}

impl BufferRpcServer {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
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

    cx.set_global(BufferRpcServer { socket_path: path });
    cx.spawn(async move |cx| {
        while let Some(request) = requests.next().await {
            let _ = cx.update(|cx| {
                let response = handlers::handle_request(request.request, cx);
                request.responder.respond(response);
            });
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
