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

pub fn socket_path_from_env_or_settings(_cx: &App) -> Option<PathBuf> {
    let path = env::var_os("ZED_BUFFER_RPC_SOCK")?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
