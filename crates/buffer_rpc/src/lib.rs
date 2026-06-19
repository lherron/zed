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
//! - `transport` — UDS accept loop, per-connection reader/writer halves, conn registry
//!   (not yet implemented).
//! - `protocol` — serde JSON-RPC 2.0 types, schema source of truth (not yet implemented).
//! - `handlers` — method dispatch to Project/BufferStore/Buffer (not yet implemented).
//! - `positions` — UTF-16 ↔ byte-offset and anchor codecs (not yet implemented).
//!
//! # Off-by-default
//! The server is disabled unless `ZED_BUFFER_RPC_SOCK` is set or the settings flag is on.
//! `socket_path_from_env_or_settings` returns `None` in that case; the `app.run` call site
//! in `main.rs` uses it as a gate.

pub mod framing;
