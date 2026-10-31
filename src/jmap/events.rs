//! Server-Sent Events parsing for the JMAP push channel (RFC 8620 §7.3).
//!
//! The wire format is [SSE]: `field: value` lines, frames separated by a blank
//! line. Only `event`, `data` and `id` carry meaning here — `id` is what a
//! reconnect replays from, via the `Last-Event-ID` header.
//!
//! [SSE]: https://html.spec.whatwg.org/multipage/server-sent-events.html

/// One complete SSE frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServerEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Most bytes held while waiting for a frame to end. Push frames are a few
/// hundred bytes; a stream that goes this far without a blank line is not
/// speaking SSE, and holding it would grow without bound.
pub const MAX_PENDING_BYTES: usize = 1024 * 1024;

/// The stream sent more than [`MAX_PENDING_BYTES`] without ending a frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Overflow;

/// Reassembles frames from arbitrary byte chunks.
///
/// Chunk boundaries fall wherever the network puts them — mid-frame, mid-line,
/// between the `\r` and `\n` of a line ending, even inside a multi-byte
/// character — so bytes are held until the blank line that terminates a frame
/// arrives, and only a whole frame is decoded.
#[derive(Default)]
pub struct EventParser {
    buf: Vec<u8>,
    /// How far `buf` is known to hold no separator, so each chunk is scanned
    /// once rather than the whole buffer again.
    scanned: usize,
}

impl EventParser {
    /// Append a chunk and return every frame it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<ServerEvent>, Overflow> {
        self.buf.extend_from_slice(chunk);

        let mut out = Vec::new();
        let mut start = 0;
        while let Some((at, len)) = next_frame_end(&self.buf[start..], self.scanned) {
            let frame = &self.buf[start..start + at];
            if let Some(event) = parse_frame(&String::from_utf8_lossy(frame)) {
                out.push(event);
            }
            start += at + len;
            self.scanned = 0;
        }
        self.buf.drain(..start);
        // A separator can straddle the next chunk, so rescan its first bytes.
        self.scanned = self.buf.len().saturating_sub(3);

        if self.buf.len() > MAX_PENDING_BYTES {
            *self = Self::default();
            return Err(Overflow);
        }
        Ok(out)
    }
}

/// Offset and length of the first frame separator at or after `from`: a blank
/// line, in either line-ending convention. Returns the earliest match so a
/// stream that mixes them cannot desynchronise.
fn next_frame_end(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let from = from.min(buf.len());
    buf[from..].windows(2).enumerate().find_map(|(i, w)| {
        let at = from + i;
        if w == b"\n\n" {
            Some((at, 2))
        } else if buf[at..].starts_with(b"\r\n\r\n") {
            Some((at, 4))
        } else {
            None
        }
    })
}

/// A frame with no recognised field is not an event — that is how keep-alive
/// comments (`: ping`) stay invisible to callers.
fn parse_frame(frame: &str) -> Option<ServerEvent> {
    let mut event = ServerEvent::default();
    let mut data = Vec::new();
    let mut recognised = false;

    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        // A line with no colon is a field with an empty value.
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event.event = Some(value.to_string()),
            "data" => data.push(value),
            "id" => event.id = Some(value.to_string()),
            // `retry` and unknown fields are ignored: reconnect backoff is the
            // caller's, and it has better information than the server does.
            _ => continue,
        }
        recognised = true;
    }

    recognised.then(|| {
        event.data = data.join("\n");
        event
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_whole_frame() {
        let mut parser = EventParser::default();
        let events = parser
            .feed(b"event: state\ndata: {\"x\":1}\nid: abc\n\n")
            .unwrap();
        assert_eq!(
            events,
            vec![ServerEvent {
                event: Some("state".into()),
                data: "{\"x\":1}".into(),
                id: Some("abc".into()),
            }]
        );
    }

    #[test]
    fn holds_a_frame_split_across_chunks() {
        let mut parser = EventParser::default();
        assert!(parser.feed(b"event: sta").unwrap().is_empty());
        assert!(parser.feed(b"te\ndata: {\"x\":1}").unwrap().is_empty());
        let events = parser.feed(b"\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"x\":1}");
    }

    #[test]
    fn holds_a_frame_split_between_cr_and_lf() {
        let mut parser = EventParser::default();
        assert!(parser.feed(b"data: hi\r\n\r").unwrap().is_empty());
        let events = parser.feed(b"\ndata: there\r\n\r\n").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hi");
        assert_eq!(events[1].data, "there");
    }

    #[test]
    fn joins_repeated_data_lines() {
        let mut parser = EventParser::default();
        let events = parser.feed(b"data: one\ndata: two\n\n").unwrap();
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn yields_several_frames_from_one_chunk() {
        let mut parser = EventParser::default();
        let events = parser.feed(b"data: a\n\ndata: b\n\ndata: c\n\n").unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn skips_comment_only_frames() {
        let mut parser = EventParser::default();
        assert!(parser.feed(b": ping\n\n").unwrap().is_empty());
    }

    #[test]
    fn holds_a_character_split_across_chunks() {
        let mut parser = EventParser::default();
        let text = "data: café\n\n".as_bytes();
        let split = text.iter().position(|&b| b == 0xc3).unwrap() + 1;
        assert!(parser.feed(&text[..split]).unwrap().is_empty());
        let events = parser.feed(&text[split..]).unwrap();
        assert_eq!(events[0].data, "café");
    }

    #[test]
    fn gives_up_on_a_stream_that_never_ends_a_frame() {
        let mut parser = EventParser::default();
        let line = vec![b'x'; 64 * 1024];
        let mut result = Ok(Vec::new());
        for _ in 0..=(MAX_PENDING_BYTES / line.len()) {
            result = parser.feed(&line);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(Overflow));
        // And starts clean afterwards.
        assert_eq!(parser.feed(b"data: ok\n\n").unwrap()[0].data, "ok");
    }

    #[test]
    fn tolerates_a_missing_space_after_the_colon() {
        let mut parser = EventParser::default();
        let events = parser.feed(b"event:state\ndata:{}\n\n").unwrap();
        assert_eq!(events[0].event.as_deref(), Some("state"));
        assert_eq!(events[0].data, "{}");
    }
}
