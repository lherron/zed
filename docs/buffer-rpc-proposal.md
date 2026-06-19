# Zed Buffer RPC — Design Proposal (Path B: in-process UDS bridge)

> **Status: conditionally approved** by daedalus@zed:primary (ruling #8961, 2026-06-19).
> The 8 constraints, the invariant, and the required tests below are folded in. No auth /
> tokens / handshakes — explicitly out of scope per the brief and the ruling.

## Goal

Let an external local process **enumerate, read, edit, save, and watch** the live
in-memory buffers (the `text` CRDT) of a running Zed instance, by embedding a small
RPC server inside a Zed **fork**. Edits go through the *same* `Buffer::edit` path the
editor itself uses, so UI, dirty state, undo, project events, and collab propagation
react naturally — we are not reimplementing the CRDT, just exposing a typed door to it.

**Caveat on side effects (constraint 6).** UI / dirty / undo / project events / collab
are buffer- and project-event driven and *do* fire for RPC edits. **LSP and autosave are
not pure buffer-open side effects:**
- LSP didOpen/didChange is only guaranteed for buffers holding an `OpenLspBufferHandle`
  (`crates/editor/src/editor.rs:10645`, `crates/project/src/project.rs:3200`). An RPC-only
  *invisible* buffer gets no LSP unless the RPC server explicitly owns such a handle for it.
  Until that's implemented, the doc/CLI contract states **hidden RPC buffers have no LSP
  guarantee**.
- Autosave is pane/item driven (`crates/workspace/src/item.rs:938`, `pane.rs:2525`), so
  hidden RPC-opened buffers **require an explicit `buffer/save`** — there is no implicit save.

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
- **Reliability bounds (constraint 5 — not auth; memory/liveness protection).** A broken or
  stalled local client must never pin memory or stall the foreground. Enforced limits:
  - `MAX_FRAME_BYTES` — reject + close on oversized `Content-Length` (before allocating).
  - `MAX_INBOUND_QUEUED` — cap pending parsed requests per connection; close on overflow.
  - `MAX_OUTBOUND_QUEUED` — cap queued notifications per connection; **drop-and-close** a
    subscriber that can't keep up rather than growing unbounded (test 8 / risk: stalled
    subscriber).
  - Malformed headers/JSON → close the connection cleanly; never panic the app.

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
| `buffer/edit` | `{bufferId, edits:[{range, newText}], baseVersion?, autoindent?, source?}` | `{version, lamport}` |
| `buffer/save` | `{bufferId}` | `{version, savedMtime}` |
| `buffer/subscribe` | `{bufferId}` | `{ok}` |
| `buffer/unsubscribe` | `{bufferId}` | `{ok}` |
| `anchor/create` | `{bufferId, position, bias}` | `{anchor}` (opaque token) |
| `anchor/resolve` | `{bufferId, anchor}` | `{position, valid}` |

### Notifications (Zed → client, after `buffer/subscribe`)

| Method | Params |
|---|---|
| `buffer/didChange` | `{bufferId, version, changes:[{range, newText}], isLocal}` — see coordinate-frame rule below |
| `buffer/didSave` | `{bufferId, version}` |
| `buffer/didOpen` | `{bufferId, path}` |
| `buffer/didClose` | `{bufferId}` |

**`didChange` coordinate frame (constraint 4 — high-severity).** `BufferEvent::Operation`
carries CRDT ops whose `EditOperation.ranges` are `FullOffset` in the operation's
**base/pre-edit** version (`EditOperation.version`, `crates/text/src/text.rs:619`). Each
`change.range` MUST be encoded in that pre-edit coordinate frame so that applying the changes
in order against the client's *previous* text exactly reproduces the new server text.
Converting ranges against the *post-edit* snapshot yields wrong patches under deletion,
multibyte UTF-16, or concurrent edits. **If range encoding cannot be made correct, M3 is
blocked** — we ship no `didChange` rather than approximate notifications.

### Workspace targeting (decided: explicit, with a `current-active` escape hatch)

Every `buffer/*` and `buffer/list` call takes a **required** `workspace` field — no
implicit active-window magic, so scripts are deterministic. It accepts either:
- a concrete `workspaceId`, or
- the literal sentinel string **`"current-active"`**, which the server resolves to the
  currently-focused window via `cx.active_window()` (`crates/gpui/src/app.rs:1129`) +
  downcast to `MultiWorkspace`.

`workspace/list` marks which entry is `active`, and `workspace/active` returns its id
directly, so clients can resolve once and pin a concrete id if they want stability across
focus changes.

**`workspaceId` is an opaque, ephemeral handle (constraint 7).** It is a runtime
`EntityId`, not a durable identity — it does not survive window/workspace churn. Clients
treat it as opaque. A **stale id fails cleanly with a typed `StaleWorkspace` error**, never
a panic and never silent fallback to another window. Use `"current-active"` or re-list when
an id goes stale.

### Position model

`position`/`range` are expressed as **UTF-16 `{line, character}`** by default (matches
JS string indexing and LSP), with an optional `{offset}` byte form. The server converts
via the snapshot's `offset_to_point_utf16` / `point_utf16_to_offset` (`crates/text/src/text.rs`).
Stable cross-edit positions use **anchors** (`crates/text/src/anchor.rs:11`), serialized
as an opaque token the client never interprets.

### Consistency (constraint 3 — hard gate)

`buffer/edit` accepts an optional `baseVersion` (an encoded `clock::Global`,
`crates/text/src/text.rs:829`). **If supplied and the current buffer version has advanced
beyond it, the edit is rejected with a typed `Conflict` error carrying the current version,
and NO mutation is performed** — never a silent rebase. The caller re-reads and retries.
Without `baseVersion`, edits apply at the given positions and merge into the CRDT. Robust
clients should pass `baseVersion` and/or address edits by **anchor**, not raw offset.

### Edit transactions & source (constraint 2)

`buffer/edit` is applied as a **single, self-contained Zed transaction** that does **not**
merge into the previous (user) undo group — one RPC `buffer/edit` call ⇒ one undo step.
Follow the existing agent edit path (`crates/agent_ui/src/buffer_codegen.rs:976`,
`crates/agent/src/tools/edit_session.rs:961`): finalize any open group, `start_transaction`,
apply `Buffer::edit`, then `end_transaction_with_source(...)`.

Edit **source** is explicit. RPC edits are emitted as **`BufferEditSource::Agent`** by
default (never `User` — that would pollute edit-prediction / action-log / undo grouping).
The protocol exposes an optional `source` field for future differentiation, but **M0–M2
default it to `Agent` and reject unsupported values**. Arbitrary client-selected sources are
**not** passed through to `BufferEditSource::User` — that would require a separate justified
reason and its own test.

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

### Threading (reuses the open-listener pattern — constraint 1)

**Strict rule: socket I/O and JSON framing NEVER run on the foreground/app thread; all
entity access runs ONLY on the foreground via `AsyncApp`/`cx.update`.** The two never cross.

- **Background thread** (`thread::spawn`, like `open_listener.rs:424`): `UnixListener::bind`,
  accept loop. Per connection, a reader parses framed JSON-RPC (subject to §1 bounds) and
  pushes `(Request, ResponderHandle)` into a global `mpsc::unbounded` sender. A writer half
  drains a per-connection response/notification channel back to the socket. **All
  encode/decode/byte work lives here.**
- **Foreground task** (`cx.spawn`, like `main.rs:973`): drains the request channel; each
  request runs inside `cx.update(|cx| …)` to touch entities on the app thread. Async ops
  (`open_buffer`, `save_buffer` return `Task<…>`) are awaited in the spawned future, then the
  *already-serializable* result is handed back to the writer half — **no framing on this
  thread.**
- **Subscriptions**: `buffer/subscribe` registers a `cx.subscribe(&buffer, …)` whose handler
  reads `BufferEvent::Operation { operation, is_local }` (`crates/language/src/buffer.rs:315`),
  extracts each `EditOperation`'s `FullOffset` ranges + `new_text` in the op's **pre-edit
  coordinate frame** (constraint 4), and pushes a `didChange` onto that connection's bounded
  writer channel (§1 `MAX_OUTBOUND_QUEUED`; drop-and-close on overflow). The serialization
  happens on the background/writer side, not the foreground (constraint 1). Connection drop
  tears down its subscriptions.

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

// buffer/open  (constraint 7: prefer the existing open_local_buffer seam over
// duplicating find_or_create_worktree). For an abs path outside all worktrees this
// seam creates the (invisible) worktree itself, so we don't re-implement that logic.
project.update(cx, |p, cx| p.open_local_buffer(abs_path, cx)) // crates/project/src/project.rs:3079 → Task<Result<Entity<Buffer>>>
// For a path already inside a worktree, use open_buffer(ProjectPath{..}) // :3170
// Note: RPC-created buffers are invisible → no project-panel clutter, and per
// constraint 6 they have NO LSP guarantee and require explicit buffer/save.

// buffer/text
let snap = buffer.read(cx).text_snapshot();      // crates/language/src/buffer.rs:1426
snap.text();                                     // crates/text/src/text.rs:2193 (full)
snap.text_for_range(start..end);                 // :2296 (range, chunked)
snap.version();                                  // :2265

// buffer/edit  — the core (constraint 2: one transaction, source = Agent, NOT User)
buffer.update(cx, |b, cx| {
    b.finalize_last_transaction(cx);             // don't merge into prior user group
    b.start_transaction();                       // crates/language/src/buffer.rs:2882
    b.edit(edits, autoindent, cx);               // :2679 — S: usize|Point|PointUtf16|Anchor
    b.end_transaction_with_source(               // signature: (source, cx); mirrors agent path
        BufferEditSource::Agent, cx);            // crates/language/src/buffer.rs:300
});
// baseVersion check happens BEFORE this block; on stale version → Conflict, no edit.

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
| M0 | Crate skeleton, UDS listener, `initialize`/ping, env-gated, **§1 bounds** | `socat - UNIX-CONNECT:$sock`; + malformed headers / oversized `Content-Length` / slow reader stay responsive (test 1) |
| M1 | Read path: `workspace/list`, `workspace/active`, `buffer/list`, `buffer/open`, `buffer/text` | multi-window list; `current-active` follows focus; stale id → typed error (test 2); open inside-worktree + abs-outside via invisible worktree (test 3) |
| M2 | Write path: `buffer/edit` (transaction + `Agent` source), `baseVersion` gate, `buffer/save` | RPC edit updates UI/dirty, undo reverts as one step, source correct (test 4); stale `baseVersion` → `Conflict`, no change (test 5); save writes file + version/mtime (test 6) |
| M3 | Notifications: `subscribe` + `didChange`/`didSave`; anchors | **gated on correct pre-edit range encoding** — insertion/deletion/astral-UTF-16 round-trip exactly (test 7); stalled subscriber hits bound + drops (test 8). If encoding can't be made correct, **M3 ships without `didChange`**. |
| M4 | `ts-rs` codegen + `@scope/zed-rpc` client package | TS script reads + edits a live buffer |
| M5 | `zedctl` CLI + end-to-end | `just install`, real forked Zed w/ `ZED_BUFFER_RPC_SOCK`, run `zedctl`/TS client for read/edit/save/watch (test 10) |

Each Rust milestone is validated against a **real built+running forked Zed**, not unit tests
(constraint/test 10 — `just install` + installed binary, never unit tests alone).

### Invariant (the predicate every accepted mutation must satisfy)

> For every accepted RPC mutation, **Zed remains the sole authority over the live buffer
> CRDT**: the mutation is applied on the app thread through existing `Project`/`Buffer` APIs,
> emits the same buffer/project events as an equivalent in-process edit batch, is **rejected
> rather than silently rebased** when its declared base version is stale, and **the transport
> cannot block the foreground thread or accumulate unbounded memory**.

### Required tests (daedalus ruling #8961)

The smoke column above maps to tests 1–8 and 10. Plus **test 9 (LSP):** if the doc ever keeps
an LSP guarantee for RPC-only buffers, add a test proving the RPC server owns an
`OpenLspBufferHandle` and LSP receives didOpen/didChange. Otherwise the doc/CLI contract
states hidden RPC buffers have no LSP guarantee until opened/registered (current stance, §Goal).

---

## 7. Trust boundary (documentation only — NO auth, constraint 8)

- Off by default; opt-in via env/setting. Socket `0600` in a user-private dir.
- **Any local process that can open the socket gets full read/write of open file contents.**
  This is documented as the intended trust model for a local internal tool.
- **No auth, tokens, or handshakes** — explicitly out of scope per the brief and the ruling.
- No network listener, ever.

---

## 8. Decisions (resolved) & remaining risks

**Resolved:**
1. **Multi-workspace addressing → explicit + `current-active` sentinel.** `workspace` is
   required on all buffer ops; pass a concrete `workspaceId` or `"current-active"`. Server also
   exposes `workspace/active` and an `active` flag on `workspace/list`. (See §2.)
2. **Opening files outside any worktree → use the `open_local_buffer` seam.** `buffer/open`
   on an absolute local path goes through `Project::open_local_buffer` (which creates the
   invisible worktree itself); direct `ProjectPath` handling is reserved for paths already
   resolved into a worktree. No duplication of `find_or_create_worktree` logic. (See §3 mapping.)
3. **First milestone depth → full, including live watch.** Ship M0–M5 (read + write + save +
   subscribe/didChange + anchors) before calling it done.

**Daedalus constraints (ruling #8961) — folded in:**
1. Foreground-only entity access; socket/JSON off the foreground → §3 threading.
2. `buffer/edit` = one transaction, source `Agent` not `User` → §2 edit transactions, §3 mapping.
3. `baseVersion` hard gate, no silent rebase → §2 consistency.
4. `didChange` ranges in pre-edit coordinate frame; block M3 if not correct → §2 notifications, §6.
5. Reliability bounds (frame/queue caps, drop/close) → §1.
6. Corrected LSP/autosave claim; hidden buffers no LSP + explicit save → §Goal.
7. Workspace ids opaque/ephemeral, stale → typed error; prefer `open_local_buffer` → §2, §3.
8. Removed the optional-token sentence → §7.

**Residual risks (severity per ruling):**
- **High** — incorrect `didChange` coordinate conversion corrupts client state under
  deletion / multibyte UTF-16 / concurrency. Mitigation: test 7 gates M3.
- **High** — unbounded queues / frame reads degrade the editor even on a private socket.
  Mitigation: §1 bounds, tests 1 & 8.
- **Medium** — overclaiming LSP/autosave for hidden buffers. Mitigation: §Goal narrowed, test 9.
- **Medium** — raw `Buffer::edit` reports `User`, polluting prediction/undo. Mitigation:
  wrap like agent path, source `Agent`, test 4.
- **Low** — `EntityId` workspace ids are runtime handles; scripts must tolerate stale ids.
  Mitigation: typed `StaleWorkspace`, test 2.

**Low-stakes implementation choices (mine, not gated):**
- UTF-16 `{line,character}` default, byte offset alternate.
- Blocking threads + channels (proven by open_listener); could move to
  `smol::Async<UnixListener>` later.
- Everything in `crates/buffer_rpc/` + one `main.rs` call site; `ts-rs` bindings pinned to a
  fork commit in the TS repo.

**Confidence (per ruling): Medium** — direction sound, but this is a *proposal* ruling; no
`crates/buffer_rpc` code exists yet. Code approval is a separate gate after M0–M2.
```
