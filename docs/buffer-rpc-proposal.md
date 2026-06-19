# Zed Buffer RPC — Design Proposal (Path B: in-process UDS bridge)

## Goal

Let an external local process **enumerate, read, edit, save, and watch** the live
in-memory buffers (the `text` CRDT) of a running Zed instance, by embedding a small
RPC server inside a Zed **fork**. Edits go through the *same* `Buffer::edit` path the
editor itself uses, so LSP, autosave, undo history, collab broadcast, and the UI all
react naturally — we are not reimplementing the CRDT, just exposing a typed door to it.

Two deliverables, two repos:

1. **Rust server** — lives in the Zed fork (`crates/buffer_rpc/` + one call-site in `main.rs`).
2. **TypeScript binding + `zedctl` CLI** — separate repo, talks to the socket.

---

## 1. Transport & framing

- **Unix domain socket**, off by default. Enabled via env var `ZED_BUFFER_RPC_SOCK`
  (explicit path) or, if unset and a setting flag is on, a default path:
  - macOS/Linux: `${XDG_RUNTIME_DIR or paths::data_dir()}/zed-{release_channel}-bufrpc.sock`
  - Socket file created `0600`, in a user-only dir. Local-only, never network.
- **Framing: JSON-RPC 2.0 over LSP-style `Content-Length` headers.**
  - Bidirectional (client requests + server-initiated `didChange` notifications).
  - The TS side reuses `vscode-jsonrpc`, which speaks exactly this framing — zero custom parser.
- **Protocol version** negotiated in `initialize`; server rejects mismatched majors.

Why JSON-RPC and not Zed's protobuf/`crates/rpc`? Because this is a *new, simple,
TS-friendly* surface — we get direct typed entity access in-process, so none of the
collab replica/room machinery is needed. JSON keeps the TS binding trivial.

---

## 2. Method surface

### Requests (client → Zed)

| Method | Params | Returns |
|---|---|---|
| `initialize` | `{clientName, protocolVersion}` | `{serverVersion, protocolVersion, pid, releaseChannel}` |
| `workspace/list` | — | `[{workspaceId, rootPaths[], active}]` |
| `workspace/active` | — | `{workspaceId}` (resolves the currently-focused window) |
| `buffer/list` | `{workspace}` | `[{bufferId, path, worktreeId, dirty, version}]` |
| `buffer/open` | `{workspace, path}` (abs or project path) | `{bufferId, path, version, text?}` |
| `buffer/text` | `{bufferId, range?}` | `{text, version}` |
| `buffer/edit` | `{bufferId, edits:[{range, newText}], baseVersion?, autoindent?}` | `{version, lamport}` |
| `buffer/save` | `{bufferId}` | `{version, savedMtime}` |
| `buffer/subscribe` | `{bufferId}` | `{ok}` |
| `buffer/unsubscribe` | `{bufferId}` | `{ok}` |
| `anchor/create` | `{bufferId, position, bias}` | `{anchor}` (opaque token) |
| `anchor/resolve` | `{bufferId, anchor}` | `{position, valid}` |

### Notifications (Zed → client, after `buffer/subscribe`)

| Method | Params |
|---|---|
| `buffer/didChange` | `{bufferId, version, changes:[{range, newText}], isLocal}` |
| `buffer/didSave` | `{bufferId, version}` |
| `buffer/didOpen` | `{bufferId, path}` |
| `buffer/didClose` | `{bufferId}` |

### Workspace targeting (decided: explicit, with a `current-active` escape hatch)

Every `buffer/*` and `buffer/list` call takes a **required** `workspace` field — no
implicit active-window magic, so scripts are deterministic. It accepts either:
- a concrete `workspaceId` (an `EntityId`, obtained from `workspace/list`), or
- the literal sentinel string **`"current-active"`**, which the server resolves to the
  currently-focused window via `cx.active_window()` (`crates/gpui/src/app.rs:1129`) +
  downcast to `MultiWorkspace`.

`workspace/list` marks which entry is `active`, and `workspace/active` returns its id
directly, so clients can resolve once and pin a concrete id if they want stability across
focus changes.

### Position model

`position`/`range` are expressed as **UTF-16 `{line, character}`** by default (matches
JS string indexing and LSP), with an optional `{offset}` byte form. The server converts
via the snapshot's `offset_to_point_utf16` / `point_utf16_to_offset` (`crates/text/src/text.rs`).
Stable cross-edit positions use **anchors** (`crates/text/src/anchor.rs:11`), serialized
as an opaque token the client never interprets.

### Consistency

`buffer/edit` accepts an optional `baseVersion` (an encoded `clock::Global`,
`crates/text/src/text.rs:829`). If supplied and the buffer has advanced, the server
**rejects with `Conflict` + current version** (caller re-reads/rebases). Without it,
edits apply at the given positions and merge into the CRDT (last-writer-wins on overlap).
Recommended pattern for robust clients: address edits by **anchor**, not offset.

---

## 3. Rust server (in the fork)

New crate **`crates/buffer_rpc/`** (keeps the diff against upstream isolated to one new
crate + a single call site, so rebasing the fork on upstream Zed stays cheap):

```
crates/buffer_rpc/
  src/lib.rs          // BufferRpcServer, Global handle, init(cx)
  src/transport.rs    // UnixListener accept loop, Content-Length framing, conn registry
  src/protocol.rs     // serde + ts-rs types — THE schema source of truth
  src/handlers.rs     // method dispatch → Project/BufferStore/Buffer calls
  src/positions.rs    // offset/point-utf16/anchor + clock::Global (version) codecs
```

### Hook point

In `crates/zed/src/main.rs`, inside the `app.run(...)` closure
(`crates/zed/src/main.rs:484`), after globals/workspace init — mirroring how the CLI
open-listener is wired (`crates/zed/src/zed/open_listener.rs:410`, set-global at
`main.rs:527`, foreground drain at `main.rs:973`):

```rust
if let Some(path) = buffer_rpc::socket_path_from_env_or_settings(cx) {
    buffer_rpc::init(path, cx); // gated; no-op when disabled
}
```

### Threading (reuses the open-listener pattern)

- **Background thread** (`thread::spawn`, like `open_listener.rs:424`): `UnixListener::bind`,
  accept loop. Per connection, a reader parses framed JSON-RPC and pushes
  `(Request, ResponderHandle)` into a global `mpsc::unbounded` sender. A writer half drains
  a per-connection response/notification channel back to the socket.
- **Foreground task** (`cx.spawn`, like `main.rs:973`): drains the request channel; each
  request runs inside `cx.update(|cx| …)` to touch entities on the app thread. Async ops
  (`open_buffer`, `save_buffer` return `Task<…>`) are awaited in the spawned future, then
  the result is sent back through the responder.
- **Subscriptions**: `buffer/subscribe` registers a `cx.subscribe(&buffer, …)` whose handler
  serializes `BufferEvent::Operation { operation, is_local }` (`crates/language/src/buffer.rs:315`)
  into a `didChange` and pushes it to that connection's writer channel. Connection drop tears
  down its subscriptions.

### Method → API mapping (all cited, verified)

```rust
// workspace/list  — enumerate open windows → workspaces → projects
cx.windows() // crates/gpui/src/app.rs:1110
  .filter_map(|w| w.downcast::<MultiWorkspace>())
// workspace.project()  → crates/workspace/src/workspace.rs:2623

// buffer/list
project.read(cx).opened_buffers(cx)              // crates/project/src/project.rs:2236
buffer.read(cx).id()                             // BufferId, crates/text/src/text.rs:72
buffer.read(cx).file().map(|f| f.full_path(cx))  // crates/language/src/buffer.rs:1432,365

// buffer/open  — decided: auto-add a worktree for abs paths outside existing ones
// 1. resolve workspace (concrete id or "current-active")
// 2. if `path` is abs and not under any worktree:
let (worktree, rel) = project
    .update(cx, |p, cx| p.find_or_create_worktree(abs_path, /*visible*/ false, cx))
    .await?;                                      // crates/project/src/project.rs (worktree_store)
let path = ProjectPath { worktree_id: worktree.read(cx).id(), path: rel };
project.update(cx, |p, cx| p.open_buffer(path, cx)) // :3170 → Task<Result<Entity<Buffer>>>
// Note: created worktree is `visible: false` so it doesn't clutter the project panel.

// buffer/text
let snap = buffer.read(cx).text_snapshot();      // crates/language/src/buffer.rs:1426
snap.text();                                     // crates/text/src/text.rs:2193 (full)
snap.text_for_range(start..end);                 // :2296 (range, chunked)
snap.version();                                  // :2265

// buffer/edit  — the core
buffer.update(cx, |b, cx| {
    b.edit(edits, autoindent, cx)                // crates/language/src/buffer.rs:2679
    //   edits: IntoIterator<(Range<S: ToOffset>, T: Into<Arc<str>>)>
    //   S can be usize | Point | PointUtf16 | Anchor   → returns Option<clock::Lamport>
});

// buffer/save
project.update(cx, |p, cx| p.save_buffer(buffer, cx)) // crates/project/src/project.rs:3293 → Task<Result<()>>

// anchors / version
snap.anchor_after(offset) / anchor_before(...)   // crates/text/src/anchor.rs
buffer.read(cx).version()                         // clock::Global, crates/text/src/text.rs:829
```

### Globals

`BufferRpcServer` stored via `impl Global` + `cx.set_global` (`crates/gpui/src/app.rs:1825`),
matching `OpenListener::set_global` usage.

---

## 4. Schema sharing (Rust ↔ TS, kept in sync)

`protocol.rs` types derive **`ts-rs`** (`#[derive(TS)] #[ts(export)]`). A `cargo test` in
the fork emits `.d.ts`/`.ts` into `crates/buffer_rpc/bindings/`. The separate TS repo
consumes these as the generated `@scope/zed-rpc-protocol` package (vendored or via a
codegen script pinned to a fork commit). One source of truth; no hand-drift between
server and client. (Alternative: `schemars` → JSON Schema → `json-schema-to-typescript`,
if we prefer a schema artifact over direct TS emission.)

---

## 5. Separate repo: TS binding + CLI

```
zed-rpc/                      (Bun workspace — Lance's default runtime)
  packages/protocol/          generated types from ts-rs (pinned to fork commit)
  packages/client/            @scope/zed-rpc  — the binding
    src/client.ts             ZedRpcClient over vscode-jsonrpc + net UDS socket
    src/discover.ts           default socket path resolver (mirrors Rust)
  packages/cli/               zedctl — built on the client
    src/index.ts              commander/yargs commands
```

### Client (`@scope/zed-rpc`)

```ts
const zed = await ZedRpcClient.connect();           // discovers socket, initializes
const buffers = await zed.listBuffers();
const { bufferId, text } = await zed.openBuffer({ path: "/abs/file.ts" });
await zed.edit(bufferId, [{ range, newText: "…" }]);
await zed.save(bufferId);
const sub = zed.onDidChange(bufferId, (e) => { /* live edits */ });
```

- Thin Promise wrapper over `vscode-jsonrpc`'s `MessageConnection` on a `net.Socket`
  connected to the UDS path. Notifications surface as typed `EventEmitter`/callbacks.
- Anchor helpers return opaque tokens; provide `createAnchor`/`resolveAnchor` for
  edit-stable positions.

### CLI (`zedctl`)

```
zedctl ls                          # list open buffers (table | --json)
zedctl cat <path> [--range a:b]    # print buffer text
zedctl edit <path> --range L:C-L:C --text "…"
zedctl apply <path> --patch file.diff      # structured multi-edit
zedctl watch <path>                # stream didChange events
zedctl save <path>
zedctl workspaces
```

Built directly on `@scope/zed-rpc`. Output: human tables by default, `--json` for piping.

---

## 6. Milestones

| # | Scope | Manual smoke test |
|---|---|---|
| M0 | Crate skeleton, UDS listener, `initialize`/ping, env-gated | `socat - UNIX-CONNECT:$sock` round-trip |
| M1 | Read path: `workspace/list`, `buffer/list`, `buffer/open`, `buffer/text` | open a file in Zed, dump it over the socket |
| M2 | Write path: `buffer/edit` (offset + utf16), `buffer/save` | edit over socket, see change land live in Zed UI + on disk after save |
| M3 | Notifications: `subscribe` + `didChange`/`didSave`; anchors | type in Zed, observe streamed changes on the socket |
| M4 | `ts-rs` codegen + `@scope/zed-rpc` client package | TS script reads + edits a live buffer |
| M5 | `zedctl` CLI + end-to-end | `zedctl edit` against a running forked Zed |

Each Rust milestone is validated against a **real built+running forked Zed**, not unit tests.

---

## 7. Security & trust boundary

- Off by default; opt-in via env/setting. Socket `0600` in a user-private dir.
- **Any local process that can open the socket gets full read/write of open file contents.**
  Document this explicitly. Optionally gate with a per-launch token echoed in `initialize`.
- No network listener, ever.

---

## 8. Decisions (resolved) & remaining risks

**Resolved:**
1. **Multi-workspace addressing → explicit + `current-active` sentinel.** `workspace` is
   required on all buffer ops; pass a concrete `workspaceId` or `"current-active"`. Server also
   exposes `workspace/active` and an `active` flag on `workspace/list`. (See §2.)
2. **Opening files outside any worktree → auto-add worktree.** `buffer/open` on an abs path
   calls `find_or_create_worktree(..., visible: false)` so any file is reachable without
   cluttering the UI. (See §3 mapping.)
3. **First milestone depth → full, including live watch.** Ship M0–M5 (read + write + save +
   subscribe/didChange + anchors) before calling it done.

**Remaining risks / low-stakes choices:**
- **UTF-16 vs byte offsets.** Defaulting to UTF-16 `{line,character}` (JS/LSP-native), byte
   offset as alternate. Settled unless you object.
- **Async UDS vs blocking threads.** Using blocking threads + channels (proven by open_listener).
   Could switch to `smol::Async<UnixListener>` on gpui's background executor later — fewer threads,
   marginally more code. Low stakes.
- **Fork maintenance.** Everything in `crates/buffer_rpc/` + one `main.rs` call site; `ts-rs`
   bindings pinned to a fork commit in the TS repo.
```
