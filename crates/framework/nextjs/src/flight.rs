//! The Flight response the App Router answers a navigation with, as a fetch
//! middleware on `text/x-component`.
//!
//! A client side navigation fetches a React Server Components Flight stream,
//! the row format the `__next_f` scripts carry inside a page, here with no
//! script wrapper and no string escaping. The rows are:
//! - `<hexId>:<json>\n`, JSON ended by a newline
//! - `<hexId>:<Tag><data>\n`, tagged, ended by a newline
//! - `<hexId>:T<hexLen>,<bytes>`, tagged `T`, length delimited, no newline
//!
//! A `T` row's length is the byte length of its content, so moving an address
//! inside it changes the length and the header is written again with the new
//! one. Core's plain replacement would leave the old length, which is why the
//! middleware moves the addresses itself and core leaves the body to it.

use std::io;

use trusted_server_core::host_rewrite::rewrite_bare_host_at_boundaries;
use trusted_server_core::middleware::{
    Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
};
use trusted_server_core::streaming_processor::StreamProcessor;

/// The name a `[[fetch]]` entry for [`FLIGHT_MEDIA_TYPE`] writes.
pub const FLIGHT_MIDDLEWARE: &str = "framework.nextjs.flight";

/// The media type of a Flight response, which the middleware works on.
pub const FLIGHT_MEDIA_TYPE: &str = "text/x-component";

/// Moves the origin's address to the publisher's in a Flight response, on
/// the responses a `[[fetch]]` entry names [`FLIGHT_MIDDLEWARE`] for.
pub(crate) struct Flight;

impl Middleware for Flight {
    fn middleware_id(&self) -> &'static str {
        FLIGHT_MIDDLEWARE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == FLIGHT_MEDIA_TYPE
    }

    fn moves_origin_address(&self) -> bool {
        true
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            stream: Some(Box::new(RscFlightUrlRewriter::new(
                context.origin_host,
                context.request_host,
                context.request_scheme,
            ))),
            ..MiddlewareAction::pass()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Id,
    Tag,
    Length,
    ChunkByNewline,
    ChunkByLength,
}

/// Rewrites the addresses inside a Flight stream row by row, writing a `T`
/// row's length again after its content changes.
///
/// The origin's `https://` and `http://` addresses move to the publisher's,
/// and a protocol relative one and a bare host at hostname boundaries to the
/// publisher's host, in one pass that never reads what it wrote. It does
/// not read JSON escapes, so `\/\/origin.example.com` is left alone, which a
/// navigation response does not carry. The escaped form inside a page's
/// `__next_f` scripts is handled by the HTML middleware.
pub(crate) struct RscFlightUrlRewriter {
    origin_host: String,
    request_url: String,
    request_host: String,
    request_protocol_relative: String,

    state: RowState,
    row_id: Vec<u8>,
    row_tag: Option<u8>,
    declared_length: usize,
    remaining_length: usize,
    row_content: Vec<u8>,
    raw_header: Vec<u8>,
}

impl RscFlightUrlRewriter {
    #[must_use]
    pub(crate) fn new(origin_host: &str, request_host: &str, request_scheme: &str) -> Self {
        Self {
            origin_host: origin_host.to_owned(),
            request_url: format!("{request_scheme}://{request_host}"),
            request_host: request_host.to_owned(),
            request_protocol_relative: format!("//{request_host}"),
            state: RowState::Id,
            row_id: Vec::new(),
            row_tag: None,
            declared_length: 0,
            remaining_length: 0,
            row_content: Vec::new(),
            raw_header: Vec::new(),
        }
    }

    fn reset_row(&mut self) {
        self.state = RowState::Id;
        self.row_id.clear();
        self.row_tag = None;
        self.declared_length = 0;
        self.remaining_length = 0;
        self.row_content.clear();
        self.raw_header.clear();
    }

    /// The row's text with each of the origin's addresses moved once, in one
    /// pass from the start, so nothing written is read again.
    ///
    /// An occurrence of the origin host after `https://`, `http://` or `//`
    /// is moved with what comes before it, and the text between those
    /// addresses is searched for a bare host at hostname boundaries.
    fn rewrite_utf8_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };

        if self.origin_host.is_empty() || !text.contains(&self.origin_host) {
            return bytes.to_vec();
        }

        let mut out = String::with_capacity(text.len());
        // The end of the last address moved, and where the next search starts.
        let mut done = 0;
        let mut search = 0;
        while let Some(found) = text[search..].find(self.origin_host.as_str()) {
            let host = search + found;
            search = host + self.origin_host.len();
            let before = &text[done..host];
            let Some((gap, replacement)) = [
                ("https://", &self.request_url),
                ("http://", &self.request_url),
                ("//", &self.request_protocol_relative),
            ]
            .into_iter()
            .find_map(|(prefix, replacement)| {
                before.strip_suffix(prefix).map(|gap| (gap, replacement))
            }) else {
                continue;
            };
            self.push_bare_hosts(&mut out, gap);
            out.push_str(replacement);
            done = search;
        }
        self.push_bare_hosts(&mut out, &text[done..]);
        out.into_bytes()
    }

    /// Appends `text` to `out` with a bare origin host at hostname boundaries
    /// moved to the publisher's host.
    fn push_bare_hosts(&self, out: &mut String, text: &str) {
        match rewrite_bare_host_at_boundaries(text, &self.origin_host, &self.request_host) {
            Some(rewritten) => out.push_str(&rewritten),
            None => out.push_str(text),
        }
    }

    fn finalize_newline_row(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.row_id);
        out.push(b':');
        if let Some(tag) = self.row_tag {
            out.push(tag);
        }
        let rewritten = self.rewrite_utf8_bytes(&self.row_content);
        out.extend_from_slice(&rewritten);
        out.push(b'\n');
        self.reset_row();
    }

    fn finalize_length_row(&mut self, out: &mut Vec<u8>) {
        let Some(tag) = self.row_tag else {
            // A length delimited row always has a tag, so this passes the
            // bytes through as they came.
            out.extend_from_slice(&self.raw_header);
            out.extend_from_slice(&self.row_content);
            self.reset_row();
            return;
        };

        out.extend_from_slice(&self.row_id);
        out.push(b':');
        out.push(tag);

        if tag == b'T' {
            let rewritten = self.rewrite_utf8_bytes(&self.row_content);
            let new_len = rewritten.len();
            out.extend_from_slice(format!("{new_len:x}").as_bytes());
            out.push(b',');
            out.extend_from_slice(&rewritten);
        } else {
            // A length delimited row of another tag is not text to rewrite.
            out.extend_from_slice(format!("{:x}", self.declared_length).as_bytes());
            out.push(b',');
            out.extend_from_slice(&self.row_content);
        }

        self.reset_row();
    }

    fn flush_partial_row(&mut self, out: &mut Vec<u8>) {
        if self.raw_header.is_empty() && self.row_content.is_empty() {
            return;
        }
        out.extend_from_slice(&self.raw_header);
        out.extend_from_slice(&self.row_content);
        self.reset_row();
    }
}

impl StreamProcessor for RscFlightUrlRewriter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        let mut out = Vec::with_capacity(chunk.len());
        let mut i = 0;

        while i < chunk.len() {
            match self.state {
                RowState::Id => {
                    let b = chunk[i];
                    i += 1;
                    if b == b':' {
                        self.raw_header.push(b':');
                        self.state = RowState::Tag;
                    } else {
                        self.row_id.push(b);
                        self.raw_header.push(b);
                    }
                }
                RowState::Tag => {
                    let b = chunk[i];
                    i += 1;

                    if b == b'T' || b == b'V' {
                        self.row_tag = Some(b);
                        self.raw_header.push(b);
                        self.state = RowState::Length;
                        self.declared_length = 0;
                    } else if b.is_ascii_uppercase() {
                        self.row_tag = Some(b);
                        self.raw_header.push(b);
                        self.state = RowState::ChunkByNewline;
                    } else {
                        // Not a tag, so the first byte of a JSON row.
                        self.row_tag = None;
                        self.row_content.push(b);
                        self.state = RowState::ChunkByNewline;
                    }
                }
                RowState::Length => {
                    let b = chunk[i];
                    i += 1;
                    if b == b',' {
                        self.raw_header.push(b',');
                        self.remaining_length = self.declared_length;
                        self.state = RowState::ChunkByLength;
                    } else {
                        self.raw_header.push(b);
                        let digit = match b {
                            b'0'..=b'9' => (b - b'0') as usize,
                            b'a'..=b'f' => (b - b'a' + 10) as usize,
                            b'A'..=b'F' => (b - b'A' + 10) as usize,
                            _ => 0,
                        };
                        self.declared_length = (self.declared_length << 4) | digit;
                    }
                }
                RowState::ChunkByNewline => {
                    let Some(pos) = chunk[i..].iter().position(|&b| b == b'\n') else {
                        self.row_content.extend_from_slice(&chunk[i..]);
                        break;
                    };
                    let end = i + pos;
                    self.row_content.extend_from_slice(&chunk[i..end]);
                    i = end + 1;
                    self.finalize_newline_row(&mut out);
                }
                RowState::ChunkByLength => {
                    let available = chunk.len() - i;
                    let take = available.min(self.remaining_length);
                    self.row_content.extend_from_slice(&chunk[i..i + take]);
                    i += take;
                    self.remaining_length -= take;

                    if self.remaining_length == 0 {
                        self.finalize_length_row(&mut out);
                    }
                }
            }
        }

        if is_last {
            self.flush_partial_row(&mut out);
        }

        Ok(out)
    }

    fn reset(&mut self) {
        self.reset_row();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use trusted_server_core::html_processor::test_support::{ORIGIN_HOST, REQUEST_HOST};
    use trusted_server_core::integrations::IntegrationDocumentState;
    use trusted_server_core::middleware::test_support::context;
    use trusted_server_core::middleware::{HTML_MEDIA_TYPE, MiddlewareChain};

    use super::*;

    fn run_rewriter(
        rewriter: &mut RscFlightUrlRewriter,
        input: &[u8],
        chunk_size: usize,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        let mut pos = 0;
        while pos < input.len() {
            let end = (pos + chunk_size).min(input.len());
            let chunk = &input[pos..end];
            let rewritten = rewriter
                .process_chunk(chunk, false)
                .expect("should process chunk");
            output.extend_from_slice(&rewritten);
            pos = end;
        }

        let tail = rewriter
            .process_chunk(&[], true)
            .expect("should process final chunk");
        output.extend_from_slice(&tail);
        output
    }

    fn rewriter() -> RscFlightUrlRewriter {
        RscFlightUrlRewriter::new("origin.example.com", "proxy.example.com", "https")
    }

    #[test]
    fn rewrites_newline_rows() {
        let input = b"0:[\"https://origin.example.com/page\"]\n";

        let output = run_rewriter(&mut rewriter(), input, 8);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");
        assert_eq!(
            output_str, "0:[\"https://proxy.example.com/page\"]\n",
            "Output should rewrite URLs in newline rows"
        );
    }

    #[test]
    fn rewrites_the_http_address_to_the_request_scheme() {
        let input = b"0:[\"http://origin.example.com/page\",\"//origin.example.com/a\"]\n";

        let output = run_rewriter(&mut rewriter(), input, 8192);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");
        assert_eq!(
            output_str, "0:[\"https://proxy.example.com/page\",\"//proxy.example.com/a\"]\n",
            "should move an http address to the request's scheme and a protocol relative one as it is"
        );
    }

    #[test]
    fn rewrites_t_rows_and_updates_length() {
        let t_content = r#"{"url":"https://origin.example.com/page"}"#;
        let json_row = "2:[\"ok\"]\n";
        let input = format!("1:T{:x},{}{}", t_content.len(), t_content, json_row);

        let output = run_rewriter(&mut rewriter(), input.as_bytes(), 7);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");

        let rewritten_t_content = r#"{"url":"https://proxy.example.com/page"}"#;
        let expected = format!(
            "1:T{:x},{}{}",
            rewritten_t_content.len(),
            rewritten_t_content,
            json_row
        );

        assert_eq!(
            output_str, expected,
            "Output should update T row lengths after rewriting"
        );
    }

    #[test]
    fn handles_t_row_header_and_body_split_across_chunks() {
        let t_content = r#"{"url":"https://origin.example.com/page"}"#;
        let input = format!("1:T{:x},{}", t_content.len(), t_content);

        // Split such that the header ends before the comma and content begins in a later chunk.
        let output = run_rewriter(&mut rewriter(), input.as_bytes(), 3);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");

        let rewritten_t_content = r#"{"url":"https://proxy.example.com/page"}"#;
        let expected = format!("1:T{:x},{}", rewritten_t_content.len(), rewritten_t_content);

        assert_eq!(
            output_str, expected,
            "Rewriter should handle T rows split across chunks"
        );
    }

    #[test]
    fn bare_host_rewrite_respects_hostname_boundaries() {
        let input = b"0:[\"cdn.origin.example.com\",\"notorigin.example.com\",\"origin.example.com.uk\",\"origin.example.com/news\",\"origin.example.com\"]\n";

        let output = run_rewriter(&mut rewriter(), input, 5);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");

        assert_eq!(
            output_str,
            "0:[\"cdn.origin.example.com\",\"notorigin.example.com\",\"origin.example.com.uk\",\"proxy.example.com/news\",\"proxy.example.com\"]\n",
            "Output should only rewrite bare host occurrences"
        );
    }

    /// A request host that begins with the origin host, as a local host named
    /// after the publisher or the origin host with a port does, is written
    /// once in every address over either scheme, in a `T` row whose length
    /// follows it and in a newline row, rather than matched again inside its
    /// own replacement.
    #[test]
    fn a_request_host_that_begins_with_the_origin_host_is_written_once() {
        let t_content =
            r#"{"a":"https://publisher.example/news","b":"http://publisher.example/old"}"#;
        let json = r#"["//publisher.example/c.png","publisher.example/bare"]"#;
        let input = format!("1:T{:x},{t_content}2:{json}\n", t_content.len());

        for request in ["publisher.example.localhost", "publisher.example:8443"] {
            for scheme in ["http", "https"] {
                let mut rewriter = RscFlightUrlRewriter::new("publisher.example", request, scheme);

                let output = run_rewriter(&mut rewriter, input.as_bytes(), 5);

                let t_rewritten = format!(
                    r#"{{"a":"{scheme}://{request}/news","b":"{scheme}://{request}/old"}}"#
                );
                let json_rewritten = format!(r#"["//{request}/c.png","{request}/bare"]"#);
                assert_eq!(
                    String::from_utf8(output).expect("should be valid UTF-8"),
                    format!(
                        "1:T{:x},{t_rewritten}2:{json_rewritten}\n",
                        t_rewritten.len()
                    ),
                    "should write {request} once over {scheme}"
                );
            }
        }
    }

    /// Hosts that do not begin with the origin host are written as before,
    /// both one with no part of the origin host in it and a `www.` host in
    /// front of an apex origin, and a longer name that holds the origin host
    /// is left alone.
    #[test]
    fn a_request_host_that_does_not_begin_with_the_origin_host_is_written_as_before() {
        for (origin, request) in [
            ("publisher.example", "publisher-example.localhost"),
            ("example.com", "www.example.com"),
        ] {
            let mut rewriter = RscFlightUrlRewriter::new(origin, request, "https");
            let row = format!(
                r#"0:["https://{origin}/a","http://{origin}/b","//{origin}/c.png","{origin}/d","cdn.{origin}/e"]"#
            );

            let output = run_rewriter(&mut rewriter, format!("{row}\n").as_bytes(), 8192);

            let expected = format!(
                r#"0:["https://{request}/a","https://{request}/b","//{request}/c.png","{request}/d","cdn.{origin}/e"]"#
            );
            assert_eq!(
                String::from_utf8(output).expect("should be valid UTF-8"),
                format!("{expected}\n"),
                "should write {request} once for {origin}"
            );
        }
    }

    #[test]
    fn each_address_is_moved_once_from_the_start() {
        let rewriter = RscFlightUrlRewriter::new("a.example", "a.example.localhost", "http");
        let rewrite = |text: &str| {
            String::from_utf8(rewriter.rewrite_utf8_bytes(text.as_bytes()))
                .expect("should be valid UTF-8")
        };

        assert_eq!(
            rewrite("https://a.example/1 http://a.example/2 //a.example/3 a.example/4"),
            "http://a.example.localhost/1 http://a.example.localhost/2 //a.example.localhost/3 a.example.localhost/4"
        );
        assert_eq!(rewrite("nothing here"), "nothing here");
        assert_eq!(rewrite(""), "");

        let no_origin = RscFlightUrlRewriter::new("", "a.example.localhost", "http");
        assert_eq!(
            no_origin.rewrite_utf8_bytes(b"https://a.example/1"),
            b"https://a.example/1".to_vec(),
            "should leave the text alone with no origin host"
        );
    }

    #[test]
    fn the_middleware_runs_on_flight_alone_in_the_fetch_phase_and_moves_the_address_itself() {
        let middleware = Flight;

        assert_eq!(middleware.middleware_id(), FLIGHT_MIDDLEWARE);
        assert!(
            middleware.handles_media_type(FLIGHT_MEDIA_TYPE),
            "should work on a Flight response"
        );
        assert!(
            !middleware.handles_media_type(HTML_MEDIA_TYPE)
                && !middleware.handles_media_type("text/plain"),
            "should work on no other media type"
        );
        assert!(
            middleware.moves_origin_address(),
            "should tell core to leave the origin's address to it"
        );
        assert_eq!(middleware.phases(), &[MiddlewarePhase::Fetch]);
    }

    #[test]
    fn the_middleware_s_plan_rewrites_a_flight_response_with_its_lengths() {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Fetch,
            FLIGHT_MEDIA_TYPE,
            vec![Arc::new(Flight) as Arc<dyn Middleware>],
        );
        let state = IntegrationDocumentState::default();
        let mut plan = chain
            .plan(&context(MiddlewarePhase::Fetch, &state))
            .expect("should plan a stream processor alone");
        assert_eq!(
            plan.processors.len(),
            1,
            "should carry one stream processor and nothing about HTML: {plan:?}"
        );
        let t_content = format!(r#"{{"url":"https://{ORIGIN_HOST}/page"}}"#);
        let input = format!(
            "1:T{:x},{t_content}2:[\"//{ORIGIN_HOST}/a\"]\n",
            t_content.len()
        );

        let mut processor = plan.processors.remove(0);
        let mut output = processor
            .process_chunk(input.as_bytes(), false)
            .expect("should process the stream");
        output.extend(
            processor
                .process_chunk(&[], true)
                .expect("should finish the stream"),
        );

        let rewritten = format!(r#"{{"url":"https://{REQUEST_HOST}/page"}}"#);
        assert_eq!(
            String::from_utf8(output).expect("should be valid UTF-8"),
            format!(
                "1:T{:x},{rewritten}2:[\"//{REQUEST_HOST}/a\"]\n",
                rewritten.len()
            ),
            "should move the origin's address and write the T row's length again"
        );
    }
}
