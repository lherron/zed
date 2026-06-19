//! Position and version codecs for the Buffer RPC wire protocol.
//!
//! This module is **GPUI-free** — all helpers operate on `&str` or
//! [`clock::Global`] without touching any live editor state.  The implementer
//! wires these into `handlers.rs`; they remain independently testable via
//! `cargo test -p buffer_rpc`.
//!
//! ## Position model (spec §2)
//!
//! Wire positions use **UTF-16 `{line, character}`** by default (matching both
//! JS string indexing and the Language Server Protocol), with an optional
//! `{offset}` byte form.  Conversion between representations is performed by
//! [`point_utf16_to_offset`] and [`offset_to_point_utf16`].
//!
//! ### UTF-16 code-unit counting rules
//! - BMP characters (U+0000–U+FFFF, including `é`, `€`, CJK): **1 UTF-16
//!   code unit** each, regardless of how many UTF-8 bytes they occupy.
//! - Astral/supplementary characters (U+10000+, e.g. emoji `😀`): **2 UTF-16
//!   code units** (a surrogate pair).  A `character` index pointing INTO the
//!   middle of a surrogate pair is out of range.
//!
//! ## Version codec
//!
//! A buffer version ([`clock::Global`]) is encoded as a flat [`WireVersion`]
//! (`Vec<u32>`) where index `i` is the highest sequence number observed for
//! replica `i`.  Trailing zeroes are stripped on encode and implied on decode.

use clock::Global;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during position conversion.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum PositionError {
    /// The requested `line` index equals or exceeds the number of lines in
    /// the buffer.
    #[error("line {line} is out of range (buffer has {line_count} line(s))")]
    LineOutOfRange { line: u32, line_count: u32 },

    /// The requested `character` index exceeds the UTF-16 length of the line
    /// (or splits a surrogate pair).
    #[error(
        "character {character} is out of range on line {line} \
         (line has {line_len} UTF-16 code unit(s))"
    )]
    CharacterOutOfRange {
        line: u32,
        character: u32,
        line_len: u32,
    },

    /// The requested byte `offset` exceeds the buffer's length.
    ///
    /// `offset == text.len()` (past-end sentinel) is **valid**; only
    /// `offset > text.len()` is rejected.
    #[error("byte offset {offset} is out of range (buffer has {len} byte(s))")]
    OffsetOutOfRange { offset: usize, len: usize },
}

// ── Wire types ────────────────────────────────────────────────────────────────

/// Wire-format position: UTF-16 `{line, character}` with an optional `{offset}`
/// byte form.
///
/// Both `line` and `character` are **0-indexed**.  When `offset` is present the
/// server uses it directly and ignores `{line, character}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// 0-indexed line number.
    pub line: u32,
    /// 0-indexed UTF-16 code-unit column within the line.
    pub character: u32,
    /// Optional byte offset in the document text.  When present, takes
    /// precedence over `{line, character}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

/// Wire-format range expressed as two [`Position`]s (both 0-indexed, inclusive
/// start / exclusive end following LSP convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Wire form of a [`clock::Global`] version vector.
///
/// A dense `Vec<u32>` where index `i` holds the highest sequence number
/// observed for replica `i`.  Trailing zeroes are stripped on encode and
/// implied as zero on decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireVersion(pub Vec<u32>);

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Convert a UTF-16 `{line, character}` pair to a **byte offset** within `text`.
///
/// # Arguments
/// * `text`      — the full document text (UTF-8).
/// * `line`      — 0-indexed line number.
/// * `character` — 0-indexed UTF-16 code-unit column within `line`.
///
/// # Errors
/// * [`PositionError::LineOutOfRange`] — `line` is not a valid line index.
/// * [`PositionError::CharacterOutOfRange`] — `character` exceeds the UTF-16
///   length of the requested line (or would split a surrogate pair).
///
/// # UTF-16 rules
/// Each BMP character counts as 1 unit; each astral character counts as 2.
/// `character == utf16_line_len` is valid (past-end sentinel on that line).
pub fn point_utf16_to_offset(
    text: &str,
    line: u32,
    character: u32,
) -> Result<usize, PositionError> {
    // Lines are delimited by '\n'; the final line may have no trailing newline.
    // `split('\n')` always yields at least one segment, so `line_count >= 1`.
    let line_count = text.split('\n').count() as u32;
    if line >= line_count {
        return Err(PositionError::LineOutOfRange { line, line_count });
    }

    // Byte offset of the first character of `line` (sum of each preceding
    // segment's byte length plus its consumed '\n').
    let line_start = text
        .split('\n')
        .take(line as usize)
        .map(|segment| segment.len() + 1)
        .sum::<usize>();

    // The content of this line, excluding its trailing '\n' (if any).
    let line_str = text[line_start..]
        .split('\n')
        .next()
        .expect("split always yields at least one element");

    let line_utf16_len: u32 = line_str.chars().map(|c| c.len_utf16() as u32).sum();
    if character > line_utf16_len {
        return Err(PositionError::CharacterOutOfRange {
            line,
            character,
            line_len: line_utf16_len,
        });
    }

    // Walk the line one Unicode scalar at a time, advancing the UTF-16
    // code-unit counter (1 for BMP, 2 for astral) until we reach `character`.
    let mut units: u32 = 0;
    for (byte_idx, c) in line_str.char_indices() {
        if units == character {
            return Ok(line_start + byte_idx);
        }
        let next = units + c.len_utf16() as u32;
        if next > character {
            // `character` lands inside a surrogate pair — not a valid boundary.
            return Err(PositionError::CharacterOutOfRange {
                line,
                character,
                line_len: line_utf16_len,
            });
        }
        units = next;
    }

    // `character == line_utf16_len`: past-end sentinel on this line.
    Ok(line_start + line_str.len())
}

/// Convert a **byte offset** within `text` to a UTF-16 `(row, column)` pair.
///
/// # Arguments
/// * `text`   — the full document text (UTF-8).
/// * `offset` — byte offset into `text`.  `offset == text.len()` (past-end) is
///   valid and returns the position one past the last character.
///
/// # Errors
/// * [`PositionError::OffsetOutOfRange`] — `offset > text.len()`.
///
/// # Returns
/// `Ok((row, col))` where both are 0-indexed and `col` is a UTF-16 code-unit
/// count from the start of `row`.
pub fn offset_to_point_utf16(text: &str, offset: usize) -> Result<(u32, u32), PositionError> {
    if offset > text.len() {
        return Err(PositionError::OffsetOutOfRange {
            offset,
            len: text.len(),
        });
    }

    // Walk scalars up to `offset`, tracking row (newlines seen) and the UTF-16
    // column within the current row.  `offset` is assumed to fall on a char
    // boundary (it comes from `point_utf16_to_offset` or a buffer snapshot).
    let mut row: u32 = 0;
    let mut col: u32 = 0;
    for (byte_idx, c) in text.char_indices() {
        if byte_idx >= offset {
            break;
        }
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += c.len_utf16() as u32;
        }
    }
    Ok((row, col))
}

// ── Version codec ─────────────────────────────────────────────────────────────

/// Encode a [`clock::Global`] version vector to its wire form.
///
/// The resulting [`WireVersion`] is a dense `Vec<u32>` — index `i` is the
/// highest sequence number seen for replica `i`.  Trailing zero entries are
/// stripped so that an all-zero / empty version serialises as `[]`.
pub fn encode_version(version: &Global) -> WireVersion {
    let mut values: Vec<u32> = version.iter().map(|timestamp| timestamp.value).collect();
    while values.last() == Some(&0) {
        values.pop();
    }
    WireVersion(values)
}

/// Decode a [`WireVersion`] back to a [`clock::Global`].
///
/// Index `i` in `wire.0` becomes a [`Lamport`] observation for replica `i`
/// with the corresponding sequence number.  Missing trailing entries imply
/// sequence 0 (not observed).
pub fn decode_version(wire: WireVersion) -> Global {
    use clock::{Lamport, ReplicaId};

    let mut version = Global::new();
    for (replica_id, seq) in wire.0.into_iter().enumerate() {
        // `observe` is a no-op for seq 0, so trailing/implicit zeros are fine.
        version.observe(Lamport {
            replica_id: ReplicaId::new(replica_id as u16),
            value: seq,
        });
    }
    version
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clock::{Lamport, ReplicaId};

    // Helper: build a Position with no byte offset.
    fn pos(line: u32, character: u32) -> Position {
        Position {
            line,
            character,
            offset: None,
        }
    }

    // ── UTF-16 → byte offset ──────────────────────────────────────────────────

    /// Pure ASCII: every char is 1 UTF-8 byte and 1 UTF-16 code unit.
    ///
    /// "hello\nworld"
    ///  line 0: h(0) e(1) l(2) l(3) o(4) \n(5)   → bytes 0-5
    ///  line 1: w(0) o(1) r(2) l(3) d(4)           → bytes 6-10
    #[test]
    fn test_ascii_point_to_offset() {
        let text = "hello\nworld";
        assert_eq!(point_utf16_to_offset(text, 0, 0), Ok(0)); // 'h'
        assert_eq!(point_utf16_to_offset(text, 0, 5), Ok(5)); // '\n' (past-end col on line 0)
        assert_eq!(point_utf16_to_offset(text, 1, 0), Ok(6)); // 'w'
        assert_eq!(point_utf16_to_offset(text, 1, 5), Ok(11)); // past end of "world"
    }

    /// Pure ASCII: byte offset → UTF-16 `(row, col)`.
    #[test]
    fn test_ascii_offset_to_point() {
        let text = "hello\nworld";
        assert_eq!(offset_to_point_utf16(text, 0), Ok((0, 0))); // 'h'
        assert_eq!(offset_to_point_utf16(text, 4), Ok((0, 4))); // 'o'
        assert_eq!(offset_to_point_utf16(text, 5), Ok((0, 5))); // '\n'
        assert_eq!(offset_to_point_utf16(text, 6), Ok((1, 0))); // 'w'
        assert_eq!(offset_to_point_utf16(text, 11), Ok((1, 5))); // past-end sentinel
    }

    /// BMP multibyte UTF-8: 'é' (U+00E9) is **2 UTF-8 bytes** but **1 UTF-16
    /// code unit**.  The column count must not double-count the second byte.
    ///
    /// "café\nend"
    ///  line 0: c(0) a(1) f(2) é(3)   → bytes 0-4, '\n' at byte 5
    ///  line 1: e(0) n(1) d(2)         → bytes 6-8
    ///
    /// Byte layout: c=0 a=1 f=2 é[0]=3 é[1]=4 \n=5 e=6 n=7 d=8
    #[test]
    fn test_bmp_multibyte_point_to_offset() {
        let text = "caf\u{00E9}\nend"; // "café\nend"
        // UTF-16 col 3 on line 0 → start of 'é' → byte 3
        assert_eq!(point_utf16_to_offset(text, 0, 3), Ok(3));
        // UTF-16 col 4 on line 0 → after 'é' → byte 5 ('\n')
        assert_eq!(point_utf16_to_offset(text, 0, 4), Ok(5));
        // line 1, col 0 → byte 6 ('e')
        assert_eq!(point_utf16_to_offset(text, 1, 0), Ok(6));
    }

    /// BMP multibyte UTF-8: byte offset → UTF-16 `(row, col)`.
    #[test]
    fn test_bmp_multibyte_offset_to_point() {
        let text = "caf\u{00E9}\nend"; // "café\nend"
        // byte 3 = first byte of 'é' → UTF-16 col 3 on line 0
        assert_eq!(offset_to_point_utf16(text, 3), Ok((0, 3)));
        // byte 5 = '\n' → UTF-16 col 4 on line 0 (past 'é')
        assert_eq!(offset_to_point_utf16(text, 5), Ok((0, 4)));
        // byte 6 = 'e' → line 1, col 0
        assert_eq!(offset_to_point_utf16(text, 6), Ok((1, 0)));
    }

    /// Astral character (surrogate pair): '😀' (U+1F600) is **4 UTF-8 bytes**
    /// and **2 UTF-16 code units**.  The character index must advance by 2
    /// across the emoji.
    ///
    /// "hi 😀\nok"
    ///  line 0: h(0) i(1) ' '(2) 😀(3,4)  → bytes 0-6, '\n' at byte 7
    ///  line 1: o(0) k(1)                   → bytes 8-9
    ///
    /// Byte layout: h=0 i=1 ' '=2 😀[0-3]=3-6 \n=7 o=8 k=9
    #[test]
    fn test_astral_point_to_offset() {
        let text = "hi \u{1F600}\nok"; // "hi 😀\nok"
        // UTF-16 col 3 → start of 😀 → byte 3
        assert_eq!(point_utf16_to_offset(text, 0, 3), Ok(3));
        // UTF-16 col 5 → after 😀 (skipped col 4 which is the low surrogate)
        //              → byte 7 ('\n')
        assert_eq!(point_utf16_to_offset(text, 0, 5), Ok(7));
        // line 1, col 0 → byte 8 ('o')
        assert_eq!(point_utf16_to_offset(text, 1, 0), Ok(8));
        // line 1, col 1 → byte 9 ('k')
        assert_eq!(point_utf16_to_offset(text, 1, 1), Ok(9));
    }

    /// Astral character: byte offset → UTF-16 `(row, col)`.
    #[test]
    fn test_astral_offset_to_point() {
        let text = "hi \u{1F600}\nok"; // "hi 😀\nok"
        // byte 3 → start of 😀 → UTF-16 (0, 3)
        assert_eq!(offset_to_point_utf16(text, 3), Ok((0, 3)));
        // byte 7 → '\n' → UTF-16 (0, 5)  (😀 occupies cols 3 and 4)
        assert_eq!(offset_to_point_utf16(text, 7), Ok((0, 5)));
        // byte 8 → 'o' → UTF-16 (1, 0)
        assert_eq!(offset_to_point_utf16(text, 8), Ok((1, 0)));
    }

    /// Multi-line buffer mixing all three character classes.
    ///
    /// ```text
    /// line 0: "a"          bytes  0-0    → '\n' at 1
    /// line 1: "café"       bytes  2-6    → '\n' at 7
    ///          c=2 a=3 f=4 é[0]=5 é[1]=6
    /// line 2: "hi 😀"     bytes  8-14   → '\n' at 15
    ///          h=8 i=9 ' '=10 😀[0-3]=11-14
    /// line 3: "" (trailing empty)
    /// ```
    #[test]
    fn test_multiline_mixed_point_to_offset() {
        let text = "a\ncaf\u{00E9}\nhi \u{1F600}\n";
        // line 2, UTF-16 col 3 → start of 😀 → byte 11
        assert_eq!(point_utf16_to_offset(text, 2, 3), Ok(11));
        // line 2, UTF-16 col 5 → after 😀 → byte 15 ('\n')
        assert_eq!(point_utf16_to_offset(text, 2, 5), Ok(15));
        // line 1, UTF-16 col 3 → start of 'é' → byte 5
        assert_eq!(point_utf16_to_offset(text, 1, 3), Ok(5));
    }

    #[test]
    fn test_multiline_mixed_offset_to_point() {
        let text = "a\ncaf\u{00E9}\nhi \u{1F600}\n";
        // byte 5 = first byte of 'é' on line 1 → UTF-16 (1, 3)
        assert_eq!(offset_to_point_utf16(text, 5), Ok((1, 3)));
        // byte 11 = start of 😀 on line 2 → UTF-16 (2, 3)
        assert_eq!(offset_to_point_utf16(text, 11), Ok((2, 3)));
        // byte 15 = '\n' at end of line 2 → UTF-16 (2, 5)
        assert_eq!(offset_to_point_utf16(text, 15), Ok((2, 5)));
    }

    // ── Out-of-range / clamping ───────────────────────────────────────────────

    /// Requesting a line beyond the last line yields [`PositionError::LineOutOfRange`].
    ///
    /// "hello\nworld" has 2 lines (indices 0 and 1); requesting line 2 is OOB.
    #[test]
    fn test_out_of_range_line() {
        let text = "hello\nworld";
        assert_eq!(
            point_utf16_to_offset(text, 2, 0),
            Err(PositionError::LineOutOfRange {
                line: 2,
                line_count: 2
            })
        );
    }

    /// Requesting a character beyond the UTF-16 length of a line yields
    /// [`PositionError::CharacterOutOfRange`].
    ///
    /// "hello" has UTF-16 length 5; character 6 is OOB (character 5 = past-end is valid).
    #[test]
    fn test_out_of_range_character() {
        let text = "hello";
        assert_eq!(
            point_utf16_to_offset(text, 0, 6),
            Err(PositionError::CharacterOutOfRange {
                line: 0,
                character: 6,
                line_len: 5
            })
        );
    }

    /// Requesting a byte offset past `text.len()` yields
    /// [`PositionError::OffsetOutOfRange`].
    ///
    /// `offset == text.len()` is a valid past-end sentinel; only
    /// `offset > text.len()` is rejected.
    #[test]
    fn test_out_of_range_offset() {
        let text = "hello"; // len = 5
        assert_eq!(
            offset_to_point_utf16(text, 6),
            Err(PositionError::OffsetOutOfRange { offset: 6, len: 5 })
        );
        // Exactly text.len() is valid.
        assert_eq!(offset_to_point_utf16(text, 5), Ok((0, 5)));
    }

    // ── Serde round-trips ─────────────────────────────────────────────────────

    /// A [`Position`] with only `{line, character}` must NOT emit an `"offset"`
    /// field in JSON.
    #[test]
    fn test_position_serde_utf16_only() {
        let p = pos(3, 7);
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("offset"),
            "\"offset\" field must be absent when None: {json}"
        );
        let decoded: Position = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, p);
    }

    /// A [`Position`] with both `{line, character}` and `{offset}` round-trips
    /// and the `"offset"` field IS present in the JSON.
    #[test]
    fn test_position_serde_with_offset() {
        let p = Position {
            line: 5,
            character: 10,
            offset: Some(1234),
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains("\"offset\":1234"),
            "\"offset\" field must be present: {json}"
        );
        let decoded: Position = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, p);
    }

    /// A [`Range`] round-trips through serde correctly.
    #[test]
    fn test_range_serde_roundtrip() {
        let r = Range {
            start: pos(0, 5),
            end: pos(2, 3),
        };
        let json = serde_json::to_string(&r).unwrap();
        let decoded: Range = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, r);
    }

    // ── Version codec ─────────────────────────────────────────────────────────

    /// An empty (default) [`Global`] encodes to an empty [`WireVersion`].
    #[test]
    fn test_encode_empty_version() {
        let v = Global::new();
        let wire = encode_version(&v);
        assert_eq!(wire, WireVersion(vec![]));
    }

    /// A populated version vector round-trips through encode → decode with
    /// [`PartialEq`] equality.
    #[test]
    fn test_version_roundtrip() {
        let mut v = Global::new();
        v.observe(Lamport {
            replica_id: ReplicaId::LOCAL,
            value: 5,
        });
        v.observe(Lamport {
            replica_id: ReplicaId::REMOTE_SERVER,
            value: 3,
        });
        let wire = encode_version(&v);
        let decoded = decode_version(wire);
        assert_eq!(decoded, v);
    }

    /// Trailing zeroes in the version vector are stripped on encode but implied
    /// on decode, so the round-trip still holds.
    #[test]
    fn test_version_wire_trailing_zeros_stripped() {
        let mut v = Global::new();
        // Observe only replica 0; replica 1 is implicitly 0.
        v.observe(Lamport {
            replica_id: ReplicaId::LOCAL,
            value: 7,
        });
        let wire = encode_version(&v);
        // Wire vec should be [7], not [7, 0].
        assert_eq!(
            wire.0.len(),
            1,
            "trailing zeros must be stripped; got: {:?}",
            wire.0
        );
        // Round-trip must still be equal.
        let decoded = decode_version(wire);
        assert_eq!(decoded, v);
    }

    /// A [`WireVersion`] (newtype around `Vec<u32>`) is itself serde-transparent.
    #[test]
    fn test_wire_version_serde() {
        let wire = WireVersion(vec![5, 3, 0, 1]);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: WireVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, wire);
    }
}
