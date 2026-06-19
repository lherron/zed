//! LSP-style `Content-Length` framing for the Buffer RPC transport layer.
//!
//! This module is **GPUI-free** — it operates on raw `&[u8]` slices and owns no
//! app-thread state.  The implementer wires [`FrameDecoder`] / [`encode_frame`] into
//! `transport.rs`; these types remain independently testable.
//!
//! # Wire format
//! ```text
//! Content-Length: <N>\r\n
//! \r\n
//! <N bytes of UTF-8 JSON body>
//! ```
//! This is exactly the framing used by the Language Server Protocol and consumed by
//! `vscode-jsonrpc` on the TypeScript client side — no custom parser needed there.
//!
//! # Reliability bounds (§1 constraint 5)
//! [`MAX_FRAME_BYTES`], [`MAX_INBOUND_QUEUED`], and [`MAX_OUTBOUND_QUEUED`] are the
//! three caps that prevent a broken/stalled local client from pinning memory or
//! stalling the foreground thread.  All three are enforced before allocation or
//! queuing occurs.

use thiserror::Error;

// ── Reliability-bound constants ───────────────────────────────────────────────

/// Maximum body size for a single frame, in bytes.
///
/// A `Content-Length` header claiming more than this many bytes is rejected
/// **before any body allocation** — the connection is closed with a
/// [`FramingError::Oversized`] error.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// Maximum number of parsed inbound requests queued per connection.
///
/// If the foreground dispatcher falls behind and the queue fills, the
/// connection is closed rather than letting the queue grow without bound.
pub const MAX_INBOUND_QUEUED: usize = 64;

/// Maximum number of outbound notifications/responses queued per connection.
///
/// A subscriber that cannot keep up is **dropped and closed** once this many
/// frames are pending — the editor is never stalled for a slow client.
pub const MAX_OUTBOUND_QUEUED: usize = 256;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single complete decoded frame — the raw body bytes (typically UTF-8 JSON).
///
/// The header has been fully consumed and validated by the time a `Frame` is
/// produced; callers need not inspect wire bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    /// The raw body bytes of the frame (the JSON-RPC message).
    pub body: Vec<u8>,
}

/// Errors from the framing layer.
///
/// All variants are typed so that callers can handle them cleanly.  The
/// transport layer closes the connection on any error; it **never panics**
/// the app in response to malformed client data.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FramingError {
    /// The `Content-Length` value exceeds [`MAX_FRAME_BYTES`].
    ///
    /// Rejected *before* any body bytes are allocated or read — the oversized
    /// claim itself is the signal; the body may not even have arrived.
    #[error("frame body too large: {claimed} bytes exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})")]
    Oversized {
        /// The body size that was claimed by the `Content-Length` header.
        claimed: usize,
    },

    /// The header block was malformed: missing `Content-Length` line, non-numeric
    /// value, non-UTF-8 content, or absent `\r\n\r\n` separator.
    ///
    /// The transport closes the connection cleanly after this error.
    #[error("malformed Content-Length header")]
    MalformedHeader,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse the numeric body-length from a complete header block.
///
/// `header_block` should be the bytes up to and including the `\r\n\r\n`
/// separator (i.e. everything before the body starts).
///
/// # Errors
/// - [`FramingError::MalformedHeader`] — if the block does not contain a
///   valid `Content-Length: <N>` line.
/// - [`FramingError::Oversized`] — if the value exceeds [`MAX_FRAME_BYTES`].
#[allow(clippy::todo)]
pub fn parse_content_length(_header_block: &[u8]) -> Result<usize, FramingError> {
    todo!(
        "parse_content_length: scan header_block for 'Content-Length: N\\r\\n', return N or error"
    )
}

/// Streaming frame decoder for the LSP `Content-Length` wire format.
///
/// Call [`push`][Self::push] each time bytes arrive from the socket.
/// The decoder accumulates bytes internally and emits complete [`Frame`]s
/// as they become available.  Partial frames — header or body — are held
/// until enough data arrives.
///
/// # Errors
/// If a [`FramingError`] is returned the decoder is in an indeterminate
/// state; the caller **must** close the connection and discard the decoder.
#[derive(Default)]
pub struct FrameDecoder {
    /// Internal ring buffer of not-yet-consumed bytes.
    // `buf` is unused in the stub — the implementer fills it in.
    #[allow(dead_code)]
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Create a new, empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `data` to the internal buffer and extract all complete frames.
    ///
    /// Returns `Ok(frames)` — an empty `Vec` means no frame is yet complete;
    /// keep feeding bytes.
    ///
    /// Returns `Err(e)` if the data is definitively invalid (oversized claim
    /// or malformed header).  The caller must close the connection on error.
    ///
    /// # Invariant
    /// Oversized frames are rejected as soon as the header is fully buffered —
    /// the body bytes need not have arrived.
    #[allow(clippy::todo)]
    pub fn push(&mut self, _data: &[u8]) -> Result<Vec<Frame>, FramingError> {
        todo!("FrameDecoder::push: append data to buf, parse frames in a loop, return them")
    }
}

/// Encode `body` as a single LSP-style Content-Length frame.
///
/// The returned bytes are exactly:
/// ```text
/// Content-Length: N\r\n\r\n<body>
/// ```
/// where `N` is `body.len()` in decimal ASCII.
#[allow(clippy::todo)]
pub fn encode_frame(_body: &[u8]) -> Vec<u8> {
    todo!("encode_frame: write 'Content-Length: N\\r\\n\\r\\n' then body")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── encode / decode round-trips ───────────────────────────────────────────

    /// A well-formed single frame encodes and then decodes back to the original body.
    #[test]
    fn test_well_formed_frame_round_trip() {
        let body = b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}";
        let encoded = encode_frame(body);
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&encoded).expect("should decode without error");
        assert_eq!(frames.len(), 1, "expected exactly one frame");
        assert_eq!(frames[0].body, body);
    }

    /// Two frames concatenated in a single push are both extracted.
    #[test]
    fn test_multiple_frames_in_buffer() {
        let body1 = b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}";
        let body2 = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"id\":2}";
        let mut wire = encode_frame(body1);
        wire.extend_from_slice(&encode_frame(body2));

        let mut dec = FrameDecoder::new();
        let frames = dec.push(&wire).expect("should decode without error");
        assert_eq!(frames.len(), 2, "expected two frames from combined push");
        assert_eq!(frames[0].body, body1, "first frame body mismatch");
        assert_eq!(frames[1].body, body2, "second frame body mismatch");
    }

    /// A header split across two separate `push` calls is reassembled correctly.
    #[test]
    fn test_header_split_across_reads() {
        let body = b"hello world";
        let encoded = encode_frame(body);
        // Split one third of the way through — likely lands inside the header.
        let split_at = encoded.len() / 3;
        let (first, second) = encoded.split_at(split_at);

        let mut dec = FrameDecoder::new();
        let partial = dec.push(first).expect("no error on first (partial) push");
        assert_eq!(partial.len(), 0, "no complete frame expected yet");

        let complete = dec.push(second).expect("no error on completing push");
        assert_eq!(complete.len(), 1, "expected one frame after second push");
        assert_eq!(complete[0].body, body);
    }

    /// A body split across two separate `push` calls is reassembled correctly.
    #[test]
    fn test_body_split_across_reads() {
        // Body is large enough that the split definitely lands inside the body.
        let body = b"a".repeat(128);
        let encoded = encode_frame(&body);
        // Find the position of \r\n\r\n and split a few bytes into the body.
        let header_end = encoded
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("encoded frame must contain \\r\\n\\r\\n")
            + 4;
        let split_at = header_end + 10; // 10 bytes into the body
        let (first, second) = encoded.split_at(split_at);

        let mut dec = FrameDecoder::new();
        let partial = dec.push(first).expect("no error on partial body push");
        assert_eq!(
            partial.len(),
            0,
            "no complete frame expected with partial body"
        );

        let complete = dec.push(second).expect("no error on body completion");
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0].body, body.as_slice());
    }

    /// `encode_frame` produces exactly `Content-Length: N\r\n\r\n<body>`.
    #[test]
    fn test_encode_produces_correct_format() {
        let body = b"test body";
        let encoded = encode_frame(body);
        let expected_header = format!("Content-Length: {}\r\n\r\n", body.len());
        assert!(
            encoded.starts_with(expected_header.as_bytes()),
            "encoded bytes must start with 'Content-Length: N\\r\\n\\r\\n'; got: {:?}",
            &encoded[..encoded.len().min(64)]
        );
        assert_eq!(
            &encoded[expected_header.len()..],
            body,
            "body bytes after header must be unchanged"
        );
    }

    /// `encode_frame` of an empty body produces `Content-Length: 0\r\n\r\n`.
    #[test]
    fn test_encode_empty_body() {
        let encoded = encode_frame(b"");
        let expected = b"Content-Length: 0\r\n\r\n";
        assert_eq!(encoded, expected);
    }

    // ── parse_content_length unit tests ──────────────────────────────────────

    /// `parse_content_length` extracts the numeric value from a well-formed header block.
    #[test]
    fn test_parse_content_length_valid() {
        let header = b"Content-Length: 42\r\n\r\n";
        let n = parse_content_length(header).expect("should parse successfully");
        assert_eq!(n, 42);
    }

    /// `parse_content_length` handles a zero-length body.
    #[test]
    fn test_parse_content_length_zero() {
        let header = b"Content-Length: 0\r\n\r\n";
        let n = parse_content_length(header).expect("should parse zero");
        assert_eq!(n, 0);
    }

    /// `parse_content_length` rejects a missing `Content-Length` header line.
    #[test]
    fn test_parse_content_length_missing() {
        let header = b"Content-Type: application/json\r\n\r\n";
        let result = parse_content_length(header);
        assert!(
            matches!(result, Err(FramingError::MalformedHeader)),
            "expected MalformedHeader for missing Content-Length, got: {result:?}"
        );
    }

    /// `parse_content_length` rejects a non-numeric value.
    #[test]
    fn test_parse_content_length_non_numeric() {
        let header = b"Content-Length: abc\r\n\r\n";
        let result = parse_content_length(header);
        assert!(
            matches!(result, Err(FramingError::MalformedHeader)),
            "expected MalformedHeader for non-numeric value, got: {result:?}"
        );
    }

    /// `parse_content_length` rejects a value exceeding `MAX_FRAME_BYTES`.
    #[test]
    fn test_parse_content_length_oversized() {
        let big = MAX_FRAME_BYTES + 1;
        let header = format!("Content-Length: {big}\r\n\r\n");
        let result = parse_content_length(header.as_bytes());
        assert!(
            matches!(result, Err(FramingError::Oversized { claimed }) if claimed == big),
            "expected Oversized {{ claimed: {big} }}, got: {result:?}"
        );
    }

    // ── §1 reliability-bound tests ────────────────────────────────────────────

    /// An oversized `Content-Length` is rejected as soon as the header is buffered,
    /// **before** any body allocation (body bytes need not have arrived).
    #[test]
    fn test_oversized_rejected_before_alloc() {
        let claimed = MAX_FRAME_BYTES + 1;
        // Send only the header line — the (huge) body has NOT arrived yet.
        let header_only = format!("Content-Length: {claimed}\r\n\r\n");
        let mut dec = FrameDecoder::new();
        let result = dec.push(header_only.as_bytes());
        assert!(
            matches!(result, Err(FramingError::Oversized { claimed: c }) if c == claimed),
            "decoder must reject oversized claim at header parse, not after body arrives; got: {result:?}"
        );
    }

    /// Exactly at the limit is accepted (boundary condition).
    #[test]
    fn test_exactly_max_frame_bytes_is_accepted_by_parse() {
        let header = format!("Content-Length: {MAX_FRAME_BYTES}\r\n\r\n");
        let result = parse_content_length(header.as_bytes());
        assert!(
            matches!(result, Ok(n) if n == MAX_FRAME_BYTES),
            "exactly MAX_FRAME_BYTES must be accepted; got: {result:?}"
        );
    }

    /// Garbage HTTP-style headers return a typed error, not a panic.
    #[test]
    fn test_malformed_header_http_style_no_panic() {
        let garbage = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let mut dec = FrameDecoder::new();
        let result = dec.push(garbage);
        assert!(
            matches!(result, Err(FramingError::MalformedHeader)),
            "expected MalformedHeader for HTTP-style header, got: {result:?}"
        );
    }

    /// Non-UTF-8 garbage returns a typed error, not a panic.
    #[test]
    fn test_malformed_header_non_utf8_no_panic() {
        // Invalid UTF-8 bytes in what would be the header position.
        let garbage = b"\xff\xfe\r\n\r\n";
        let mut dec = FrameDecoder::new();
        let result = dec.push(garbage);
        assert!(
            matches!(result, Err(FramingError::MalformedHeader)),
            "expected MalformedHeader for non-UTF-8 garbage, got: {result:?}"
        );
    }

    /// Completely random binary noise returns a typed error, not a panic.
    #[test]
    fn test_malformed_header_random_binary_no_panic() {
        let noise: Vec<u8> = (0u8..=127).collect();
        let mut dec = FrameDecoder::new();
        // Either an error or 0 frames (still waiting for \r\n\r\n separator).
        // The key constraint: must NOT panic.
        let _ = dec.push(&noise);
        // If we add the separator, must produce a typed error (no Content-Length).
        let mut garbage = noise.clone();
        garbage.extend_from_slice(b"\r\n\r\n");
        let result = dec.push(&garbage);
        assert!(
            matches!(result, Err(FramingError::MalformedHeader)),
            "binary noise + CRLFCRLF must be MalformedHeader, got: {result:?}"
        );
    }

    // ── queue-cap constant sanity tests ───────────────────────────────────────

    /// The queue-cap constants are positive and within reasonable bounds.
    #[test]
    fn test_queue_cap_consts_are_sane() {
        assert!(MAX_INBOUND_QUEUED > 0, "inbound queue cap must be positive");
        assert!(
            MAX_OUTBOUND_QUEUED > 0,
            "outbound queue cap must be positive"
        );
        // Not so large they defeat the memory-bounding purpose.
        assert!(
            MAX_INBOUND_QUEUED <= 4096,
            "MAX_INBOUND_QUEUED={MAX_INBOUND_QUEUED} should be a sane cap, not unbounded"
        );
        assert!(
            MAX_OUTBOUND_QUEUED <= 4096,
            "MAX_OUTBOUND_QUEUED={MAX_OUTBOUND_QUEUED} should be a sane cap, not unbounded"
        );
    }

    /// `MAX_FRAME_BYTES` is positive and under a generous but finite ceiling (1 GiB).
    #[test]
    fn test_max_frame_bytes_is_sane() {
        assert!(MAX_FRAME_BYTES > 0, "MAX_FRAME_BYTES must be positive");
        assert!(
            MAX_FRAME_BYTES <= 1024 * 1024 * 1024,
            "MAX_FRAME_BYTES={MAX_FRAME_BYTES} is unreasonably large"
        );
    }
}
