//! A minimal, pure, provider-neutral Server-Sent-Events byte framer, shared by
//! the Anthropic (T-5.2) and OpenAI (T-5.3) streaming clients.
//!
//! Both the Messages API and the Responses API stream `text/event-stream`: each
//! event is a block of `field: value` lines terminated by a blank line, and the
//! payload we care about is the (possibly multi-line) `data:` field. The two
//! providers differ only in the JSON shape of that payload - the framing is
//! identical, so it lives here once.

/// Feed it raw response chunks (BYTES, as they come off the socket); get back
/// each event's concatenated `data:` payload (one `String` per
/// blank-line-delimited event). Pure and incremental.
///
/// It buffers RAW BYTES, never a per-chunk lossy decode: `reqwest`'s `chunk()`
/// splits on arbitrary transport boundaries, so a multibyte UTF-8 codepoint (in
/// streamed text or tool-input JSON) can straddle two chunks. We decode only a
/// COMPLETE event block - which always ends on the ASCII blank-line delimiter, so
/// it is a whole number of codepoints - and a partial trailing sequence stays
/// buffered as bytes until its continuation arrives. Both LF (`\n\n`) and CRLF
/// (`\r\n\r\n`) blank-line separators are recognized; [`extract_data`]'s
/// `str::lines()` then strips any per-line `\r`.
#[derive(Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = next_event_end(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..end).collect();
            // The block ends on the ASCII blank-line delimiter, so it never cuts a
            // codepoint; lossy decode here can only ever be a true no-op.
            let text = String::from_utf8_lossy(&block);
            if let Some(data) = extract_data(&text) {
                out.push(data);
            }
        }
        out
    }
}

/// Index one past the end of the first complete SSE event in `buf` (including its
/// terminating blank line), recognizing both `\n\n` and `\r\n\r\n` separators.
fn next_event_end(buf: &[u8]) -> Option<usize> {
    let lf = find_subseq(buf, b"\n\n").map(|i| i + 2);
    let crlf = find_subseq(buf, b"\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Index of the first occurrence of `needle` in `haystack`. Shared with the
/// tests' loopback mock server (header-block scanning).
pub(crate) fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Pull the (possibly multi-line) `data:` payload out of one SSE event block.
fn extract_data(block: &str) -> Option<String> {
    let mut data = String::new();
    let mut found = false;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if found {
                data.push('\n');
            }
            // A single optional leading space after the colon is part of the
            // framing, not the payload.
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            found = true;
        }
    }
    found.then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_reassembles_an_event_split_across_chunks() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"event: message_st").is_empty());
        assert!(d.push(b"art\ndata: {\"type\":\"mess").is_empty());
        let out = d.push(b"age_start\"}\n\n");
        assert_eq!(out, vec!["{\"type\":\"message_start\"}".to_string()]);
    }

    #[test]
    fn decoder_handles_crlf_and_multiple_events_in_one_chunk() {
        let mut d = SseDecoder::default();
        let raw = "event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
        let out = d.push(raw.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "{\"type\":\"message_stop\"}");
    }

    #[test]
    fn decoder_preserves_a_multibyte_codepoint_split_across_chunks() {
        // A delta carrying a non-ASCII char ("é" = 0xC3 0xA9) whose bytes are torn
        // across two `chunk()` boundaries must NOT be corrupted into U+FFFD. This
        // is the case a per-chunk from_utf8_lossy silently destroyed.
        let payload = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"café\"}}\n\n";
        let bytes = payload.as_bytes();
        // Find a split point INSIDE the 'é' (its first byte 0xC3).
        let split = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut d = SseDecoder::default();
        assert!(
            d.push(&bytes[..split]).is_empty(),
            "partial codepoint must stay buffered"
        );
        let out = d.push(&bytes[split..]);
        assert_eq!(out.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["delta"]["text"], "café");
        assert!(
            !out[0].contains('\u{FFFD}'),
            "no replacement char: {}",
            out[0]
        );
    }

    #[test]
    fn multi_line_data_payload_is_concatenated_with_newlines() {
        // The SSE spec allows multiple `data:` lines in one event; they join with
        // '\n'. (The Responses API keeps payloads single-line, but the framer must
        // honor the spec so a wrapped payload is never silently truncated.)
        let mut d = SseDecoder::default();
        let out = d.push(b"event: x\ndata: line1\ndata: line2\n\n");
        assert_eq!(out, vec!["line1\nline2".to_string()]);
    }
}
