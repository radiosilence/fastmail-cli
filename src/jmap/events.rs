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

/// Reassembles frames from arbitrary byte chunks.
///
/// Chunk boundaries fall wherever the network puts them — mid-frame, mid-line,
/// even between the `\r` and `\n` of a line ending — so partial input is held
/// until the blank line that terminates a frame actually arrives.
#[derive(Default)]
pub struct EventParser {
    buf: String,
}

impl EventParser {
    /// Append a chunk and return every frame it completed.
    pub fn feed(&mut self, chunk: &str) -> Vec<ServerEvent> {
        self.buf.push_str(chunk);

        let mut out = Vec::new();
        while let Some((at, len)) = next_frame_end(&self.buf) {
            let frame: String = self.buf.drain(..at + len).collect();
            if let Some(event) = parse_frame(&frame) {
                out.push(event);
            }
        }
        out
    }
}

/// Offset and length of the first frame separator: a blank line, in either
/// line-ending convention. Returns the earliest match so a stream that mixes
/// them cannot desynchronise.
fn next_frame_end(buf: &str) -> Option<(usize, usize)> {
    let lf = buf.find("\n\n").map(|at| (at, 2));
    let crlf = buf.find("\r\n\r\n").map(|at| (at, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (found, None) | (None, found) => found,
    }
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
        let events = parser.feed("event: state\ndata: {\"x\":1}\nid: abc\n\n");
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
        assert!(parser.feed("event: sta").is_empty());
        assert!(parser.feed("te\ndata: {\"x\":1}").is_empty());
        let events = parser.feed("\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"x\":1}");
    }

    #[test]
    fn holds_a_frame_split_between_cr_and_lf() {
        let mut parser = EventParser::default();
        assert!(parser.feed("data: hi\r\n\r").is_empty());
        let events = parser.feed("\ndata: there\r\n\r\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hi");
        assert_eq!(events[1].data, "there");
    }

    #[test]
    fn joins_repeated_data_lines() {
        let mut parser = EventParser::default();
        let events = parser.feed("data: one\ndata: two\n\n");
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn yields_several_frames_from_one_chunk() {
        let mut parser = EventParser::default();
        let events = parser.feed("data: a\n\ndata: b\n\ndata: c\n\n");
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn skips_comment_only_frames() {
        let mut parser = EventParser::default();
        assert!(parser.feed(": ping\n\n").is_empty());
    }

    #[test]
    fn tolerates_a_missing_space_after_the_colon() {
        let mut parser = EventParser::default();
        let events = parser.feed("event:state\ndata:{}\n\n");
        assert_eq!(events[0].event.as_deref(), Some("state"));
        assert_eq!(events[0].data, "{}");
    }
}
