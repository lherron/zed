//! Codec for `buffer/didChange` notifications and anchor round-trips (M3 TDD seam).
//!
//! This module is **GPUI-free** — all helpers operate on [`text::BufferSnapshot`]
//! and [`text::Anchor`] directly, making `cargo test -p buffer_rpc` fast.
//!
//! ## `encode_changes` (constraint 4 — THE M3 gate)
//!
//! [`encode_changes`] turns a [`text::EditOperation`] into a [`Vec<WireChange>`]
//! where every `change.range` is expressed in the operation's **pre-edit/base
//! coordinate frame** (UTF-16 `{line, character}`), so that applying the changes
//! **in order** against the client's *previous* text exactly reproduces the new
//! server text.
//!
//! Converting against the *post-edit* snapshot is **WRONG** under deletion,
//! multibyte UTF-16, or concurrent edits (spec §2, constraint 4, HIGH severity /
//! HARD GATE).  If this invariant cannot be made correct, M3 ships without
//! `didChange` rather than approximate notifications.
//!
//! ## Anchor token codec
//!
//! [`encode_anchor`] / [`decode_anchor`] serialise a [`text::Anchor`] to/from an
//! opaque wire token the client treats as a black box.  The underlying CRDT anchor
//! is content-stable — it tracks text across edits — so resolving a decoded token
//! in a later snapshot yields the *moved* position.
//!
//! ## Spec references
//! - `docs/buffer-rpc-proposal.md` §2 (didChange coordinate frame), §3 (subscriptions)
//! - Tests 7 (coordinate-frame correctness) and 8 (stalled-subscriber drop)

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::positions::Range;

// ── Wire types ─────────────────────────────────────────────────────────────────

/// One element of the `changes` array in a `buffer/didChange` notification.
///
/// `range` is in the **pre-edit/base coordinate frame** (UTF-16 `{line,
/// character}`); `new_text` is the replacement text (empty = pure deletion).
///
/// Applying the changes **in order** against the client's previous text
/// exactly reproduces the new server text (spec §2, constraint 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WireChange {
    /// Range in the pre-edit text expressed as UTF-16 `{line, character}` positions.
    pub range: Range,
    /// Replacement text (empty string = pure deletion).
    pub new_text: String,
}

/// Opaque wire token representing a [`text::Anchor`].
///
/// The client treats this as a black box; the server encodes/decodes it via
/// [`encode_anchor`] / [`decode_anchor`].  The token is a base64url-encoded
/// fixed-width byte array derived from `Anchor::opaque_id()` (20 bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct WireAnchorToken(pub String);

// ── Errors ─────────────────────────────────────────────────────────────────────

/// Error decoding a [`WireAnchorToken`] back to a [`text::Anchor`].
#[derive(Debug, Clone, PartialEq, Error)]
pub enum AnchorDecodeError {
    /// Token string is not valid base64url.
    #[error("anchor token has invalid base64: {0}")]
    InvalidBase64(String),

    /// Decoded byte slice is not exactly 20 bytes.
    #[error("anchor token has wrong byte length (expected 20, got {0})")]
    BadLength(usize),
}

// ── Public API ─────────────────────────────────────────────────────────────────
//
// ALL bodies are `unimplemented!()` stubs — compile but panic at runtime.
// The RED tests below call these functions and must FAIL (via the panic).
// When the M3 implementer fills in correct bodies the RED tests must turn GREEN
// without any change to the test bodies.

/// Encode a [`text::EditOperation`] as a sequence of [`WireChange`]s in the
/// operation's **pre-edit/base coordinate frame**.
///
/// # Correctness contract (constraint 4 — HARD GATE)
///
/// Each `WireChange.range` MUST be expressed as UTF-16 `{line, character}`
/// positions into the text of `pre_edit` (the buffer snapshot **before** `op`
/// was applied).  Applying the returned changes **in order** against the pre-edit
/// text string MUST yield exactly the post-edit buffer text.
///
/// Ranges derived from the *post-edit* snapshot are **wrong** — they produce
/// bad patches under deletion, multibyte UTF-16 characters, or concurrent CRDT
/// operations from other replicas.
///
/// `pre_edit` must be the [`text::BufferSnapshot`] captured immediately before
/// `op` was applied; for local edits `op.version == pre_edit.version()`.
///
/// # Implementation notes (for M3 implementer)
///
/// `op.ranges` is a `Vec<Range<text::FullOffset>>`.  A [`text::FullOffset`]
/// counts **all** bytes (visible + deleted) in the fragment tree at the
/// operation's base version.  Converting to a visible byte offset in `pre_edit`
/// requires a walk of `pre_edit`'s fragment tree at that version context —
/// **do not** use `offset_to_point_utf16` on the FullOffset value directly.
pub fn encode_changes(
    _pre_edit: &text::BufferSnapshot,
    _op: &text::EditOperation,
) -> Vec<WireChange> {
    unimplemented!("M3: encode EditOperation FullOffset ranges → pre-edit UTF-16 WireChange vec")
}

/// Encode a [`text::Anchor`] to its opaque wire token.
///
/// Uses `Anchor::opaque_id()` (20 deterministic bytes) encoded as URL-safe
/// base64 without padding.  The client stores the resulting [`WireAnchorToken`]
/// and passes it back verbatim to `anchor/resolve`.
pub fn encode_anchor(_anchor: &text::Anchor) -> WireAnchorToken {
    unimplemented!("M3: base64url-encode Anchor::opaque_id() → WireAnchorToken")
}

/// Decode a [`WireAnchorToken`] back to a [`text::Anchor`].
///
/// Returns [`AnchorDecodeError`] if the token is not valid base64url or the
/// decoded byte array is not exactly 20 bytes (matching `Anchor::opaque_id()`).
pub fn decode_anchor(_token: &WireAnchorToken) -> Result<text::Anchor, AnchorDecodeError> {
    unimplemented!("M3: base64url-decode token → reconstruct Anchor from 20-byte opaque_id layout")
}

// ── Tests (RED — must FAIL until M3 impl fills in the stubs) ──────────────────
//
// Each test calls a stub function and will panic with `unimplemented!()`.
// This is the expected RED state.  When M3 is implemented:
//   - All RED tests must turn GREEN without modifying test bodies.
//   - All 69 existing tests must remain GREEN.
//
// Run to confirm RED:
//   cargo test -p buffer_rpc notify
#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::point_utf16_to_offset;
    use clock::ReplicaId;
    use text::{Buffer, BufferId, ToOffset};

    // ── Test helpers ───────────────────────────────────────────────────────────

    fn make_buffer(text: &str) -> Buffer {
        Buffer::new(ReplicaId::LOCAL, BufferId::new(1).unwrap(), text)
    }

    /// Apply a single [`WireChange`] to `pre_text` and return the result string.
    ///
    /// This is the client-side reconstruction step: the `change.range` is in
    /// UTF-16 pre-edit coordinates; replace that span with `change.new_text`.
    /// If [`encode_changes`] is correct, the result equals the post-edit buffer
    /// text.
    fn apply_single(pre_text: &str, change: &WireChange) -> String {
        let start = point_utf16_to_offset(
            pre_text,
            change.range.start.line,
            change.range.start.character,
        )
        .expect("start position must be valid in pre-edit text");
        let end =
            point_utf16_to_offset(pre_text, change.range.end.line, change.range.end.character)
                .expect("end position must be valid in pre-edit text");
        let mut result = pre_text.to_string();
        result.replace_range(start..end, &change.new_text);
        result
    }

    /// Apply a [`Vec<WireChange>`] to `pre_text` in reverse order (last range
    /// first) so that earlier byte offsets are not invalidated.
    ///
    /// All ranges must be in the **original** pre-edit coordinate frame — this
    /// helper enforces the requirement by converting each range against the
    /// unmodified `pre_text` and then applying from the end.
    fn apply_changes(pre_text: &str, changes: &[WireChange]) -> String {
        // Convert all ranges to byte offsets in the original text.
        let mut byte_ranges: Vec<(usize, usize, &str)> = changes
            .iter()
            .map(|c| {
                let s =
                    point_utf16_to_offset(pre_text, c.range.start.line, c.range.start.character)
                        .expect("start position must be valid in pre-edit text");
                let e = point_utf16_to_offset(pre_text, c.range.end.line, c.range.end.character)
                    .expect("end position must be valid in pre-edit text");
                (s, e, c.new_text.as_str())
            })
            .collect();
        // Apply from last (highest offset) to first so earlier byte indices stay valid.
        byte_ranges.sort_by(|a, b| b.0.cmp(&a.0));
        let mut result = pre_text.to_string();
        for (start, end, new_text) in byte_ranges {
            result.replace_range(start..end, new_text);
        }
        result
    }

    // ── encode_changes: coordinate-frame correctness (test 7) ─────────────────
    //
    // Each test: build buffer from known text, snapshot it (pre-edit), apply an
    // edit, extract the EditOperation, call encode_changes (STUB → panics), then
    // apply the changes to the pre-edit text and assert equality with post-edit
    // text.  The assert is never reached in the RED state — the panic IS the
    // expected failure.

    /// T7a — pure insertion.
    ///
    /// pre:  "hello world"
    /// edit: insert "beautiful " at byte offset 6
    /// post: "hello beautiful world"
    ///
    /// Expected single WireChange: range {(0,6)..(0,6)}, new_text="beautiful "
    /// Apply: "hello " + "beautiful " + "world" = "hello beautiful world"
    #[test]
    fn test_encode_changes_insertion() {
        let mut buf = make_buffer("hello world");
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text();

        let op = buf.edit([(6..6, "beautiful ")]);
        let edit_op = op.as_edit().expect("edit must produce an EditOperation");
        let post_text = buf.text();

        // STUB: panics with unimplemented! → RED
        let changes = encode_changes(&pre_snap, edit_op);

        // These assertions are the GREEN contract (never reached in RED state).
        assert_eq!(changes.len(), 1, "single-range insert → one WireChange");
        let reconstructed = apply_single(&pre_text, &changes[0]);
        assert_eq!(
            reconstructed, post_text,
            "applying WireChange to pre-edit text must reproduce post-edit text"
        );
    }

    /// T7b — pure deletion.
    ///
    /// pre:  "hello world"
    /// edit: delete bytes 6..11 ("world")
    /// post: "hello "
    ///
    /// Expected single WireChange: range {(0,6)..(0,11)}, new_text=""
    #[test]
    fn test_encode_changes_deletion() {
        let mut buf = make_buffer("hello world");
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text();

        let op = buf.edit([(6..11, "")]);
        let edit_op = op.as_edit().expect("edit must produce an EditOperation");
        let post_text = buf.text();

        // STUB: panics → RED
        let changes = encode_changes(&pre_snap, edit_op);

        assert_eq!(changes.len(), 1, "single-range delete → one WireChange");
        let reconstructed = apply_single(&pre_text, &changes[0]);
        assert_eq!(reconstructed, post_text);
    }

    /// T7c — replacement spanning a BMP multibyte boundary (UTF-8 multibyte,
    /// single UTF-16 code unit).
    ///
    /// pre:  "café"   — c(0) a(1) f(2) é(3..4 UTF-8, col 3 UTF-16)
    /// edit: replace é (bytes 3..5, which is 1 UTF-8 char = 2 bytes) with "e"
    /// post: "cafe"
    ///
    /// CORRECT WireChange: range {(0,3)..(0,4)}, new_text="e"
    ///   (UTF-16 end col is 4 because é is 1 UTF-16 unit)
    /// WRONG (post-edit conversion): range {(0,3)..(0,5)} — treats byte 5 as col
    ///
    /// This test pins the UTF-16 coordinate frame so a post-edit conversion fails.
    #[test]
    fn test_encode_changes_bmp_multibyte_replacement() {
        // "café": c=0, a=1, f=2, é=3 (2 UTF-8 bytes but 1 UTF-16 code unit)
        let mut buf = make_buffer("caf\u{00E9}"); // "café"
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text();

        // Replace the 2-byte 'é' (bytes 3..5) with ASCII 'e' (1 byte).
        let op = buf.edit([(3..5, "e")]);
        let edit_op = op.as_edit().expect("edit must produce an EditOperation");
        let post_text = buf.text();

        assert_eq!(post_text, "cafe", "sanity: post-edit text must be 'cafe'");

        // STUB: panics → RED
        let changes = encode_changes(&pre_snap, edit_op);

        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        // UTF-16 range must be col 3..4 (NOT 3..5, which would be the raw byte end).
        assert_eq!(
            change.range.end.character, 4,
            "end character must be 4 (UTF-16 col after é), not 5 (raw byte offset)"
        );
        let reconstructed = apply_single(&pre_text, change);
        assert_eq!(
            reconstructed, post_text,
            "applying BMP-multibyte WireChange to pre-edit text must reproduce post-edit text"
        );
    }

    /// T7d — replacement of an astral / surrogate-pair character (U+10000+).
    ///
    /// pre:  "hi 😀"   — h(0) i(1) ' '(2) 😀(3,4 — 4 UTF-8 bytes, 2 UTF-16 units)
    /// edit: replace 😀 (bytes 3..7) with "!"
    /// post: "hi !"
    ///
    /// CORRECT WireChange: range {(0,3)..(0,5)}, new_text="!"
    ///   (UTF-16 end col is 5 because 😀 occupies surrogate pair cols 3 AND 4)
    /// WRONG (post-edit or byte-offset): range {(0,3)..(0,4)} or {(0,3)..(0,7)}
    ///
    /// This is the strongest coordinate-frame gate: a wrong (byte or post-edit)
    /// conversion yields a different end character and fails the equality check.
    #[test]
    fn test_encode_changes_astral_replacement() {
        // "hi 😀": h=0,i=1,' '=2, 😀=bytes 3-6, '\n' not present
        let mut buf = make_buffer("hi \u{1F600}"); // "hi 😀"
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text();

        // Replace the 4-byte emoji (bytes 3..7) with "!".
        let op = buf.edit([(3..7, "!")]);
        let edit_op = op.as_edit().expect("edit must produce an EditOperation");
        let post_text = buf.text();

        assert_eq!(post_text, "hi !", "sanity: post-edit text must be 'hi !'");

        // STUB: panics → RED
        let changes = encode_changes(&pre_snap, edit_op);

        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        // UTF-16: 😀 occupies cols 3 and 4, so end col after it is 5.
        assert_eq!(
            change.range.start.character, 3,
            "start character must be 3 (start of 😀 in UTF-16)"
        );
        assert_eq!(
            change.range.end.character, 5,
            "end character must be 5 (UTF-16 col after 😀 surrogate pair), not 4 or 7"
        );
        let reconstructed = apply_single(&pre_text, change);
        assert_eq!(
            reconstructed, post_text,
            "applying astral WireChange to pre-edit text must reproduce post-edit text"
        );
    }

    /// T7e — encode_changes for a deletion followed by a re-insertion at the
    /// same logical position.
    ///
    /// This tests that FullOffset-to-visible-offset conversion is correct when
    /// the fragment tree contains deleted fragments (i.e. FullOffset ≠
    /// visible_offset).
    ///
    /// pre:  "foo bar"
    /// edit1 (applied first): delete "foo" (bytes 0..3) → " bar"
    /// edit2: insert "baz" at byte 0 → "baz bar"
    ///
    /// For edit2, the FullOffset of position 0 in the visible text is NOT 0 in
    /// the full fragment tree (the deleted "foo" fragment sits before it), so
    /// the pre-edit visible offset and FullOffset diverge.  encode_changes must
    /// resolve the FullOffset correctly to yield a WireChange with range
    /// {(0,0)..(0,0)} and new_text "baz".
    #[test]
    fn test_encode_changes_after_prior_deletion_full_offset_diverges() {
        let mut buf = make_buffer("foo bar");
        // First edit: delete "foo" (bytes 0..3).
        buf.edit([(0..3, "")]);
        // Now visible text is " bar".

        // Snapshot BEFORE the second edit (pre-edit for encode_changes).
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text(); // " bar"

        // Second edit: insert "baz" at visible position 0 of " bar".
        let op2 = buf.edit([(0..0, "baz")]);
        let edit_op2 = op2.as_edit().expect("edit2 must produce an EditOperation");
        let post_text = buf.text(); // "baz bar"

        assert_eq!(
            post_text, "baz bar",
            "sanity: post-edit text must be 'baz bar'"
        );

        // STUB: panics → RED (and that's exactly what we want)
        let changes = encode_changes(&pre_snap, edit_op2);

        // GREEN contract (never reached while RED):
        assert_eq!(changes.len(), 1, "single insertion → one WireChange");
        let change = &changes[0];
        assert_eq!(
            change.range.start.character, 0,
            "insertion at start → start character 0"
        );
        assert_eq!(
            change.range.end.character, 0,
            "pure insertion → zero-width range"
        );
        assert_eq!(change.new_text, "baz", "new_text must be 'baz'");
        let reconstructed = apply_single(&pre_text, change);
        assert_eq!(
            reconstructed, post_text,
            "applying WireChange after prior deletion must reproduce post-edit text"
        );
    }

    // ── Anchor codec round-trip (anchor/create + anchor/resolve) ──────────────

    /// T8a — encode/decode round-trip: the decoded anchor is byte-for-byte
    /// identical to the original (all fields preserved).
    ///
    /// anchor.opaque_id() → base64 token → decode → same 20 bytes → same Anchor.
    #[test]
    fn test_anchor_encode_decode_roundtrip() {
        let buf = make_buffer("hello world");
        let snap = buf.snapshot();

        // Anchor in the middle of the text (Bias::Right — stays after "hello ").
        let anchor = snap.anchor_after(6usize);

        // STUB: encode_anchor panics → RED
        let token = encode_anchor(&anchor);

        // STUB: decode_anchor also panics → RED (if encode were implemented)
        let decoded = decode_anchor(&token).expect("decode must succeed for a valid token");

        assert_eq!(
            anchor.opaque_id(),
            decoded.opaque_id(),
            "decoded anchor must have the same opaque identity as the original"
        );
    }

    /// T8b — anchor tracks an insertion: after inserting text before the anchor
    /// position, resolving the decoded anchor in the post-edit snapshot yields
    /// the MOVED position (not the original position).
    ///
    /// This proves that the encode/decode faithfully preserves the CRDT anchor
    /// so the built-in tracking (fragment tree → Anchor::to_offset) still works.
    ///
    /// pre:  "hello world"  — anchor_after(6) → "hello |world"  (offset 6)
    /// edit: insert "beautiful " at offset 0 → "beautiful hello world"
    /// post: anchor resolves to offset 16 ("beautiful hello |world")
    #[test]
    fn test_anchor_resolves_to_moved_position_after_edit() {
        let mut buf = make_buffer("hello world");
        let pre_snap = buf.snapshot().clone();

        // Anchor just after "hello " — will track the 'w' of "world".
        let anchor = pre_snap.anchor_after(6usize);

        // Insert "beautiful " at the start.
        buf.edit([(0..0, "beautiful ")]);
        let post_snap = buf.snapshot().clone();

        assert_eq!(buf.text(), "beautiful hello world", "sanity");

        // STUB: encode_anchor panics → RED
        let token = encode_anchor(&anchor);

        // STUB: decode_anchor panics → RED
        let decoded = decode_anchor(&token).expect("decode must succeed");

        // The decoded anchor must resolve to offset 16 in the post-edit snapshot
        // ("beautiful " is 10 chars, so old offset 6 → new offset 16).
        let resolved_offset = decoded.to_offset(&post_snap);
        assert_eq!(
            resolved_offset, 16,
            "anchor must track the insertion: resolved offset must be 16, not 6"
        );
    }

    /// T8c — anchor at the start of the buffer (min anchor special case).
    ///
    /// `anchor_before(0)` returns `Anchor::min_for_buffer(...)`.  Encoding and
    /// decoding this sentinel must round-trip correctly.
    #[test]
    fn test_anchor_min_roundtrip() {
        let buf = make_buffer("abc");
        let snap = buf.snapshot();

        let anchor = snap.anchor_before(0usize);
        assert!(anchor.is_min(), "anchor_before(0) must be the min anchor");

        // STUB → RED
        let token = encode_anchor(&anchor);
        let decoded = decode_anchor(&token).expect("decode must succeed");

        assert_eq!(
            anchor.opaque_id(),
            decoded.opaque_id(),
            "min anchor round-trip must preserve opaque identity"
        );
    }
}
