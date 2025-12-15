use std::io;

use crate::streaming_processor::StreamProcessor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Id,
    Tag,
    Length,
    ChunkByNewline,
    ChunkByLength,
}

/// Rewrites URLs inside a React Server Components (RSC) Flight stream.
///
/// Next.js (App Router) uses `react-server-dom-webpack` ("Flight") for navigation responses
/// and for inlined `__next_f` data. The wire format is a sequence of rows:
/// - `<hexId>:<json>\n` (JSON terminated by `\n`)
/// - `<hexId>:<Tag><data>\n` (tagged, terminated by `\n`)
/// - `<hexId>:T<hexLen>,<bytes...>` (tagged by `T`, length-delimited, **no trailing newline**)
///
/// For `T` rows, the length prefix is the UTF-8 byte length of the content bytes. If we rewrite
/// URLs inside the content, we must recompute the length and rewrite the header.
pub struct RscFlightUrlRewriter {
    origin_url: String,
    origin_http_url: Option<String>,
    origin_host: String,
    origin_protocol_relative: String,
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
    pub fn new(
        origin_host: &str,
        origin_url: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        // Normalize because some configs include a trailing slash (e.g. `https://origin/`).
        // If we keep the trailing slash, replacing `origin_url` inside `origin_url + "/path"`
        // would drop the delimiter and yield `https://proxyhostpath`.
        let origin_url = origin_url.trim_end_matches('/');

        let request_url = format!("{request_scheme}://{request_host}");
        let origin_protocol_relative = format!("//{origin_host}");
        let request_protocol_relative = format!("//{request_host}");

        let origin_http_url = origin_url
            .strip_prefix("https://")
            .map(|rest| format!("http://{rest}"));

        Self {
            origin_url: origin_url.to_string(),
            origin_http_url,
            origin_host: origin_host.to_string(),
            origin_protocol_relative,
            request_url,
            request_host: request_host.to_string(),
            request_protocol_relative,
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

    fn rewrite_utf8_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };

        if !text.contains(&self.origin_host) && !text.contains(&self.origin_url) {
            if let Some(http_url) = &self.origin_http_url {
                if !text.contains(http_url) {
                    return bytes.to_vec();
                }
            } else {
                return bytes.to_vec();
            }
        }

        // Keep replacement semantics consistent with `create_url_replacer`.
        let mut rewritten = text.replace(&self.origin_url, &self.request_url);
        if let Some(http_url) = &self.origin_http_url {
            rewritten = rewritten.replace(http_url, &self.request_url);
        }
        rewritten = rewritten.replace(
            &self.origin_protocol_relative,
            &self.request_protocol_relative,
        );
        rewritten = rewritten.replace(&self.origin_host, &self.request_host);

        rewritten.into_bytes()
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
            // Should never happen for length-delimited rows; fall back to passthrough.
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
            // Length-delimited row type we don't transform (e.g., future/binary Flight types).
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
                        // Not a recognized tag; treat as first byte of a JSON row.
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
                    i = end + 1; // Skip '\n'
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

    #[test]
    fn rewrites_newline_rows() {
        let input = b"0:[\"https://origin.example.com/page\"]\n";

        let mut rewriter = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com",
            "proxy.example.com",
            "https",
        );

        let output = run_rewriter(&mut rewriter, input, 8);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");
        assert_eq!(
            output_str, "0:[\"https://proxy.example.com/page\"]\n",
            "Output should rewrite URLs in newline rows"
        );
    }

    #[test]
    fn rewrites_newline_rows_with_trailing_slash_origin_url() {
        let input = b"0:[\"https://origin.example.com/page\"]\n";

        let mut rewriter = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com/",
            "proxy.example.com",
            "https",
        );

        let output = run_rewriter(&mut rewriter, input, 8);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");
        assert_eq!(
            output_str, "0:[\"https://proxy.example.com/page\"]\n",
            "Output should rewrite URLs without dropping the path slash"
        );
    }

    #[test]
    fn rewrites_t_rows_and_updates_length() {
        let t_content = r#"{"url":"https://origin.example.com/page"}"#;
        let json_row = "2:[\"ok\"]\n";
        let input = format!("1:T{:x},{}{}", t_content.len(), t_content, json_row);

        let mut rewriter = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com",
            "proxy.example.com",
            "https",
        );

        let output = run_rewriter(&mut rewriter, input.as_bytes(), 7);
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
    fn rewrites_t_rows_with_trailing_slash_origin_url() {
        let t_content = r#"{"url":"https://origin.example.com/page"}"#;
        let json_row = "2:[\"ok\"]\n";
        let input = format!("1:T{:x},{}{}", t_content.len(), t_content, json_row);

        let mut rewriter = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com/",
            "proxy.example.com",
            "https",
        );

        let output = run_rewriter(&mut rewriter, input.as_bytes(), 7);
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
            "Output should update T row lengths after rewriting without dropping the path slash"
        );
    }

    #[test]
    fn handles_t_row_header_and_body_split_across_chunks() {
        let t_content = r#"{"url":"https://origin.example.com/page"}"#;
        let input = format!("1:T{:x},{}", t_content.len(), t_content);

        let mut rewriter = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com",
            "proxy.example.com",
            "https",
        );

        // Split such that the header ends before the comma and content begins in a later chunk.
        let output = run_rewriter(&mut rewriter, input.as_bytes(), 3);
        let output_str = String::from_utf8(output).expect("should be valid UTF-8");

        let rewritten_t_content = r#"{"url":"https://proxy.example.com/page"}"#;
        let expected = format!("1:T{:x},{}", rewritten_t_content.len(), rewritten_t_content,);

        assert_eq!(
            output_str, expected,
            "Rewriter should handle T rows split across chunks"
        );
    }
}
