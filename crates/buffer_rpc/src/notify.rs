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

use crate::positions::{Position, Range};

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

/// Encode the edits between `last_sent_version` and `current`'s version as a
/// sequence of [`WireChange`]s in the **pre-edit / client coordinate frame**
/// (constraint 4 — the M3 gate).
///
/// `current` is the buffer snapshot captured **after** the mutation (so it must
/// be taken inside a post-mutation event such as [`language::BufferEvent::Edited`],
/// never inside an `Operation` callback, which fires before `apply_ops` mutates
/// the text). `last_sent_version` is the version the subscriber is known to hold
/// (its delivery cursor — see the subscription handler).
///
/// # Correctness contract (constraint 4 — HARD GATE)
///
/// Each `WireChange.range` is the `old` side of a [`text::Edit`] produced by
/// [`text::BufferSnapshot::edits_since`] — i.e. UTF-16 `{line, character}`
/// positions in the text the subscriber currently holds (`last_sent_version`).
/// `new_text` is read from `current` over the edit's `new` range.  Applying the
/// returned changes against the held text reproduces `current`'s text exactly.
///
/// This is exact under deletion, multibyte/astral UTF-16, multi-edit batches,
/// and concurrent CRDT operations because `edits_since` performs the
/// version-aware fragment-tree diff internally — we never treat a raw
/// `FullOffset` as a visible byte offset.
///
/// # Wire ordering convention
///
/// All ranges are in the client's current (pre-batch) frame. The returned
/// changes are ordered **descending by start position**, so a client may apply
/// them in array order against its held text (replacing higher offsets first
/// keeps lower offsets valid). Equivalently they may be applied as a single
/// simultaneous batch.
pub fn encode_changes_since(
    current: &text::BufferSnapshot,
    last_sent_version: &clock::Global,
) -> Vec<WireChange> {
    use text::PointUtf16;

    let mut changes: Vec<WireChange> = current
        .edits_since::<PointUtf16>(last_sent_version)
        .map(|edit| {
            let new_text: String = current
                .text_for_range(edit.new.start..edit.new.end)
                .collect();
            WireChange {
                range: Range {
                    start: Position {
                        line: edit.old.start.row,
                        character: edit.old.start.column,
                        offset: None,
                    },
                    end: Position {
                        line: edit.old.end.row,
                        character: edit.old.end.column,
                        offset: None,
                    },
                },
                new_text,
            }
        })
        .collect();

    // Descending by start position so naive in-order application is correct.
    changes.reverse();
    changes
}

/// Encode a [`text::Anchor`] to its opaque wire token.
///
/// Uses `Anchor::opaque_id()` (20 deterministic bytes) encoded as URL-safe
/// base64 without padding.  The client stores the resulting [`WireAnchorToken`]
/// and passes it back verbatim to `anchor/resolve`.
pub fn encode_anchor(anchor: &text::Anchor) -> WireAnchorToken {
    use base64::Engine as _;
    let bytes = anchor.opaque_id();
    WireAnchorToken(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Decode a [`WireAnchorToken`] back to a [`text::Anchor`].
///
/// Returns [`AnchorDecodeError`] if the token is not valid base64url or the
/// decoded byte array is not exactly 20 bytes (matching `Anchor::opaque_id()`).
///
/// The 20-byte layout mirrors `Anchor::opaque_id()` (all little-endian):
/// `[0..8]` `buffer_id` (`u64`), `[8..12]` `offset` (`u32`), `[12..16]`
/// `timestamp.value` (`Seq`/`u32`), `[16..18]` `timestamp.replica_id` (`u16`),
/// `[18]` `bias` (`0` = Left, else Right), `[19]` unused.
pub fn decode_anchor(token: &WireAnchorToken) -> Result<text::Anchor, AnchorDecodeError> {
    use base64::Engine as _;

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.0.as_bytes())
        .map_err(|error| AnchorDecodeError::InvalidBase64(error.to_string()))?;
    let bytes: [u8; 20] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| AnchorDecodeError::BadLength(bytes.len()))?;

    let buffer_id_raw = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let offset = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let value = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let replica_id = u16::from_le_bytes(bytes[16..18].try_into().unwrap());
    let bias = if bytes[18] == 0 {
        text::Bias::Left
    } else {
        text::Bias::Right
    };

    // A zero buffer id can never come from a real `Anchor` (`BufferId` is a
    // `NonZeroU64`); treat it as a malformed token rather than panicking.
    let buffer_id =
        text::BufferId::new(buffer_id_raw).map_err(|_| AnchorDecodeError::BadLength(20))?;

    let timestamp = clock::Lamport {
        replica_id: clock::ReplicaId::new(replica_id),
        value,
    };
    Ok(text::Anchor::new(timestamp, offset, bias, buffer_id))
}

// ── Tests (M3 green — encode_changes_since + anchor codec) ───────────────────
//
// The didChange seam was re-pointed (daedalus ruling, Option C) from the
// originally-stubbed `encode_changes(pre_edit, op)` to
// `encode_changes_since(current_snapshot, last_sent_version)`: the assertions
// are unchanged; only the inputs reflect the version-diff design.
//
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
        byte_ranges.sort_by_key(|b| std::cmp::Reverse(b.0));
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

        buf.edit([(6..6, "beautiful ")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text();

        let changes = encode_changes_since(&post_snap, pre_snap.version());

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

        buf.edit([(6..11, "")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text();

        let changes = encode_changes_since(&post_snap, pre_snap.version());

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
        buf.edit([(3..5, "e")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text();

        assert_eq!(post_text, "cafe", "sanity: post-edit text must be 'cafe'");

        let changes = encode_changes_since(&post_snap, pre_snap.version());

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
        buf.edit([(3..7, "!")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text();

        assert_eq!(post_text, "hi !", "sanity: post-edit text must be 'hi !'");

        let changes = encode_changes_since(&post_snap, pre_snap.version());

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
        buf.edit([(0..0, "baz")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text(); // "baz bar"

        assert_eq!(
            post_text, "baz bar",
            "sanity: post-edit text must be 'baz bar'"
        );

        // The delivery cursor is the pre-edit version (" bar"); the FullOffset of
        // the insertion diverges from the visible offset, but edits_since does the
        // version-aware diff so the wire range is correctly (0,0)..(0,0).
        let changes = encode_changes_since(&post_snap, pre_snap.version());

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

    /// Multi-edit (daedalus test 2) — two separated ranges in one batch.
    ///
    /// pre:  "hello world foo"
    /// edits: replace (0..5,"HI") and (12..15,"BAR") in one transaction
    /// post: "HI world BAR"
    ///
    /// Ranges are in the client (pre-edit) frame. The wire order must be
    /// DESCENDING by start so naive in-order application reconstructs exactly,
    /// AND the frame-agnostic `apply_changes` helper (re-sorts) must also agree.
    #[test]
    fn test_encode_changes_multi_edit_descending_order() {
        let mut buf = make_buffer("hello world foo");
        let pre_snap = buf.snapshot().clone();
        let pre_text = pre_snap.text();

        buf.edit([(0..5, "HI"), (12..15, "BAR")]);
        let post_snap = buf.snapshot().clone();
        let post_text = post_snap.text();
        assert_eq!(post_text, "HI world BAR", "sanity");

        let changes = encode_changes_since(&post_snap, pre_snap.version());
        assert_eq!(changes.len(), 2, "two separated ranges → two WireChanges");

        // Wire order must be descending by start position.
        let starts: Vec<(u32, u32)> = changes
            .iter()
            .map(|c| (c.range.start.line, c.range.start.character))
            .collect();
        assert!(
            starts[0] > starts[1],
            "changes must be ordered descending by start: {starts:?}"
        );

        // Naive in-order application against the held (pre-edit) text is exact.
        let mut naive = pre_text.clone();
        for change in &changes {
            let start = point_utf16_to_offset(
                &naive,
                change.range.start.line,
                change.range.start.character,
            )
            .unwrap();
            let end =
                point_utf16_to_offset(&naive, change.range.end.line, change.range.end.character)
                    .unwrap();
            naive.replace_range(start..end, &change.new_text);
        }
        assert_eq!(
            naive, post_text,
            "in-order (descending) application is exact"
        );

        // Frame-agnostic batch reconstruction also agrees.
        assert_eq!(apply_changes(&pre_text, &changes), post_text);
    }

    /// Remote apply_ops (daedalus test 3) — didChange computed AFTER mutation.
    ///
    /// An edit made on replica A is shipped to replica B via `apply_ops`. The
    /// post-mutation snapshot of B, diffed against B's pre-mutation version,
    /// reconstructs B's new text exactly — proving the seam is correct for
    /// remote/collab ops (the handler sets isLocal=false from the event source).
    #[test]
    fn test_encode_changes_remote_apply_ops_after_mutation() {
        use text::Operation;

        // Two replicas of the same buffer, same base text. A's edit (its op has
        // an empty base version, trivially observed by B) ships to B unchanged.
        let mut buf_a = make_buffer("shared text");
        let mut buf_b = Buffer::new(ReplicaId::new(1), BufferId::new(1).unwrap(), "shared text");

        // A deletes "shared " (bytes 0..7); B starts from the same text.
        let op = buf_a.edit([(0..7, "")]);

        let pre_snap_b = buf_b.snapshot().clone();
        let pre_text_b = pre_snap_b.text();
        buf_b.apply_ops([Operation::Edit(op.as_edit().unwrap().clone())]);
        let post_snap_b = buf_b.snapshot().clone();
        let post_text_b = post_snap_b.text();
        assert_eq!(
            post_text_b, "text",
            "sanity: B reflects the remote deletion"
        );

        let changes = encode_changes_since(&post_snap_b, pre_snap_b.version());
        assert_eq!(
            apply_changes(&pre_text_b, &changes),
            post_text_b,
            "remote op reconstructs exactly when diffed after mutation"
        );
    }

    /// Delivery cursor (daedalus test 4a) — successive edits V0→V1→V2 with the
    /// cursor advancing each step produce no duplicated or skipped content.
    #[test]
    fn test_encode_changes_cursor_advances_no_dup_or_skip() {
        let mut buf = make_buffer("one two three");

        let v0 = buf.version();
        let text_v0 = buf.text();

        buf.edit([(0..3, "ONE")]); // "ONE two three"
        let snap_v1 = buf.snapshot().clone();
        let v1 = snap_v1.version().clone();
        let text_v1 = snap_v1.text();

        buf.edit([(8..11, "THR")]); // "ONE two THRee"
        let snap_v2 = buf.snapshot().clone();
        let text_v2 = snap_v2.text();

        // First delivery: V0 → V1.
        let changes_a = encode_changes_since(&snap_v1, &v0);
        assert_eq!(
            apply_changes(&text_v0, &changes_a),
            text_v1,
            "V0→V1 reconstructs V1"
        );

        // Cursor advanced to V1; second delivery diffs only V1 → V2.
        let changes_b = encode_changes_since(&snap_v2, &v1);
        assert_eq!(
            apply_changes(&text_v1, &changes_b),
            text_v2,
            "V1→V2 reconstructs V2 with no re-send of the first edit"
        );
        // The second delivery must NOT include the first edit's region again.
        assert!(
            changes_b.iter().all(|c| c.new_text != "ONE"),
            "advancing the cursor must not re-deliver the V0→V1 edit"
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
