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
//! # Always-on with a fixed default socket
//! The server is always enabled. `socket_path_from_env_or_settings` always
//! returns a `PathBuf`: a non-empty `ZED_BUFFER_RPC_SOCK` overrides
//! (dev/multi-instance), otherwise it falls back to the fixed default
//! `$HOME/praesidium/var/run/zed-buffer-rpc.sock` (mirroring the HRC socket
//! convention). The `app.run` call site in `main.rs` initializes the server
//! unconditionally with the resolved path.

use std::{collections::HashMap, env, ffi::OsString, path::PathBuf};

use futures::StreamExt;
use gpui::{App, Entity, Global, Subscription};
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
    /// Live `buffer/subscribe` registrations, keyed by `(conn_id, buffer_id)`.
    ///
    /// Holding the gpui [`Subscription`]s here keeps them active; removing an
    /// entry drops them and unsubscribes. Connection teardown
    /// ([`BufferRpcServer::remove_connection`]) clears every entry for that
    /// connection so a dropped subscriber stops receiving notifications.
    subscriptions: HashMap<(u64, u64), SubscriptionEntry>,
}

struct OpenedBuffer {
    buffer: Entity<Buffer>,
    project: Entity<project::Project>,
}

/// The gpui subscriptions backing one `(conn, buffer)` subscription: buffer
/// events (didChange/didSave) and the entity-release observer (didClose).
pub struct SubscriptionEntry {
    pub event: Subscription,
    pub release: Subscription,
}

impl Global for BufferRpcServer {}

impl BufferRpcServer {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Register a `(conn, buffer)` subscription, keeping its gpui subscriptions
    /// alive. Replaces any prior registration for the same key.
    pub fn register_subscription(
        &mut self,
        conn_id: u64,
        buffer_id: u64,
        entry: SubscriptionEntry,
    ) {
        self.subscriptions.insert((conn_id, buffer_id), entry);
    }

    /// Drop a single `(conn, buffer)` subscription. Returns whether one existed.
    pub fn remove_subscription(&mut self, conn_id: u64, buffer_id: u64) -> bool {
        self.subscriptions.remove(&(conn_id, buffer_id)).is_some()
    }

    /// Drop every subscription belonging to a connection (called on disconnect).
    pub fn remove_connection(&mut self, conn_id: u64) {
        self.subscriptions.retain(|(conn, _), _| *conn != conn_id);
    }

    /// Whether a `(conn, buffer)` subscription is currently registered.
    pub fn is_subscribed(&self, conn_id: u64, buffer_id: u64) -> bool {
        self.subscriptions.contains_key(&(conn_id, buffer_id))
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
        subscriptions: HashMap::new(),
    });
    cx.spawn(async move |cx| {
        // Foreground drain task (constraint 1): entity access happens via
        // `cx.update` inside `handlers::dispatch`; async buffer opens are awaited
        // here in the spawned future.  No socket I/O or JSON framing on this
        // thread — the transport's writer half owns all wire encoding.
        while let Some(incoming) = requests.next().await {
            match incoming {
                transport::Incoming::Request(envelope) => {
                    let transport::RequestEnvelope {
                        request,
                        responder,
                        conn_id,
                        sink,
                        ..
                    } = envelope;
                    let conn = handlers::ConnCtx { conn_id, sink };
                    let response = handlers::dispatch(request, &conn, cx).await;
                    responder.respond(response);
                }
                transport::Incoming::Disconnected { conn_id } => {
                    let _ = cx.update(|cx| {
                        if cx.has_global::<BufferRpcServer>() {
                            cx.global_mut::<BufferRpcServer>()
                                .remove_connection(conn_id);
                        }
                    });
                }
            }
        }
    })
    .detach();
}

/// Resolve the Buffer RPC socket path. Always returns a path (never `None`):
/// the server is always-on.
///
/// - A non-empty `ZED_BUFFER_RPC_SOCK` overrides (dev / multi-instance).
/// - Otherwise the fixed default `$HOME/praesidium/var/run/zed-buffer-rpc.sock`,
///   mirroring the HRC socket convention (`~/praesidium/var/run/hrc/hrc.sock`).
pub fn socket_path_from_env_or_settings(_cx: &App) -> PathBuf {
    resolve_socket_path(env::var_os("ZED_BUFFER_RPC_SOCK"), env::var_os("HOME"))
}

/// The default socket path under `$HOME` when no override is given.
fn default_socket_path(home: Option<OsString>) -> PathBuf {
    // `$HOME` is always set in practice; fall back to the current directory so
    // we still yield a usable relative path rather than panicking.
    let home = home
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("praesidium")
        .join("var")
        .join("run")
        .join("zed-buffer-rpc.sock")
}

/// Pure resolution logic, factored out so it can be unit-tested without an `App`.
/// An empty `ZED_BUFFER_RPC_SOCK` is treated as absent (uses the default).
fn resolve_socket_path(env_sock: Option<OsString>, home: Option<OsString>) -> PathBuf {
    match env_sock {
        Some(sock) if !sock.is_empty() => PathBuf::from(sock),
        _ => default_socket_path(home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_env_overrides() {
        let resolved = resolve_socket_path(
            Some(OsString::from("/tmp/custom-zed-rpc.sock")),
            Some(OsString::from("/home/someone")),
        );
        assert_eq!(resolved, PathBuf::from("/tmp/custom-zed-rpc.sock"));
    }

    #[test]
    fn empty_env_uses_default() {
        let resolved = resolve_socket_path(
            Some(OsString::from("")),
            Some(OsString::from("/home/someone")),
        );
        assert_eq!(
            resolved,
            PathBuf::from("/home/someone/praesidium/var/run/zed-buffer-rpc.sock")
        );
    }

    #[test]
    fn unset_env_uses_default() {
        let resolved = resolve_socket_path(None, Some(OsString::from("/home/someone")));
        assert_eq!(
            resolved,
            PathBuf::from("/home/someone/praesidium/var/run/zed-buffer-rpc.sock")
        );
    }

    #[test]
    fn missing_home_falls_back_to_relative_default() {
        let resolved = resolve_socket_path(None, None);
        assert_eq!(
            resolved,
            PathBuf::from("./praesidium/var/run/zed-buffer-rpc.sock")
        );
    }
}
