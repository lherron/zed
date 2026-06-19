# Handoff — Zed Buffer RPC (Path B) — STATUS 2026-06-19 (late)

**Agent:** clod@zed:primary (coordinator, agent-tasker) · **Branch:** `buffer-rpc` · pushed to `origin/buffer-rpc`

## TL;DR
All FOUR Rust milestones (M0–M3) are implemented, live-verified, and **daedalus code-gate approved**.
The TS client + `zedctl` (M4/M5, separate `zed-rpc` repo, owned by clod@zed-rpc) shipped the M0–M2
surface and is now adding the `watch`/anchor fast-follow. One human-GUI item remains deferred.

## Spec (frozen)
`docs/buffer-rpc-proposal.md` — design, constraints, invariant, tests. No-auth by design (do NOT add auth).

## Fork state — commits on `buffer-rpc` (all pushed)
- `b65100565d` M0 — crate skeleton, UDS transport, Content-Length framing, §1 bounds, initialize/ping, main.rs:528 gated hook, thin justfile (run/build/bufrpc-run/install).
- `4d24699182` M1 — read path: workspace/list+active, buffer/list+open+text, positions UTF-16 codec, StaleWorkspace. (Fixed a WeakEntity drop bug.)
- `a724a181c7` M2 — write path: buffer/edit (one txn + BufferEditSource::Agent), baseVersion Conflict gate, buffer/save.
- `802e4e9275` M2 gate fix — buffer/open non-abs paths via Project::find_project_path (not first-root); unresolved → INVALID_PARAMS. (daedalus #9009 blocker.)
- `cc901858a2` M4a — ts-rs codegen; bindings in `crates/buffer_rpc/bindings/`.
- `8bbf0a02db` M3 — subscribe/didChange (Option C: `current.edits_since::<PointUtf16>(last_sent)`, post-mutation `Edited` trigger, delivery-cursor, atomic subscribe baseline, descending-by-start changes), didSave/didOpen/didClose, anchors (base64url opaque token). **ZERO crates/text change — fork isolation intact.** Full-surface bindings regenerated.

`cargo test -p buffer_rpc` = 96 green. clippy clean. `cargo build -p zed` ok. Isolation: only `crates/buffer_rpc/` + 3-line `main.rs` hook + `crates/zed/Cargo.toml` dep + root `Cargo.toml`/`justfile`.

## Approvals (daedalus@zed:primary)
- Architecture: #8961/#8966/#8972. Execution plan: #8982. M0–M2 code gate: **approved at 802e4e9275** (#9014). M3 Option-C ruling: #9032. M3 code gate: **approved at 8bbf0a02db** (#9040).
- Standing rule (Lance): every daedalus gate brief forbids auth gates / security theater.

## zed-rpc repo (clod@zed-rpc:primary) — M4/M5
- M0–M2 client + `zedctl` shipped at commit `889fea9` (wrkq zed-rpc T-04954/T-04955), bindings vendored from pin cc901858a2, e2e'd vs live forked Zed.
- **In flight:** re-vendor pin `8bbf0a02db` + add `zedctl watch` + anchor client methods + watch e2e. Reach: `hrcchat dm clod@zed-rpc:primary`.

## REMAINING
ALL MILESTONES COMPLETE + e2e verified.
- ✅ zed-rpc M4/M5 shipped: client + `zedctl` (read/edit/save/watch/anchor) @ `a5f9019`, vendored pin `8bbf0a02db`.
- ✅ **Final consolidated e2e (clod, independent)**: `zedctl` workspaces/cat/edit/save/ls + `watch --text` against a real `cargo run -p zed` — read/edit/save exact, watch live-streamed didChange with exact astral/multibyte reconstruction matching authoritative buffer/text.

- ✅ **Test 2 (current-active / focus-follow) — DONE** via ghostmux (real Ghostty/Aqua session) + cody (computer-use) once the Mac was unlocked. Against socket-owner PID 72006 with two windows: A focused → workspace/active=4294967388 (A active:true); B focused → 4294967824 (B active:true, A false); back to A → flips to A. current-active resolves only when a window is key. Focus-follow proven. Test instances torn down.

Still open (non-blocking, tracked):
1. **Residual stress matrix (daedalus)**: large-buffer watch; total-connection-count bound (per-conn queues already bounded). Not blockers; future hardening.

## wrkq (zed project, container buffer-rpc)
T-04947 M0 ✅ · T-04948 M1 ✅ · T-04949 M2 ✅ · T-04952 gate-fix ✅ · T-04953 M4a ✅ · T-04950 M3 ✅.

## SOP reminders
Validate against the real installed/running binary (cargo run -p zed), nc -U (no socat on host), not cargo test alone. Never push to zed-industries/zed (pre-push hook guards). Work on `buffer-rpc`.
