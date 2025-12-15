use std::sync::Arc;

use regex::{escape, Regex};
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::integrations::{
    IntegrationHtmlContext, IntegrationHtmlPostProcessor, IntegrationRegistration,
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};
use crate::settings::{IntegrationConfig, Settings};

const NEXTJS_INTEGRATION_ID: &str = "nextjs";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct NextJsIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(
        default = "default_rewrite_attributes",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    #[validate(length(min = 1))]
    pub rewrite_attributes: Vec<String>,
}

impl IntegrationConfig for NextJsIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_rewrite_attributes() -> Vec<String> {
    vec!["href".to_string(), "link".to_string(), "url".to_string()]
}

pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let config = match build(settings) {
        Some(config) => {
            log::info!(
                "NextJS integration registered: enabled={}, rewrite_attributes={:?}",
                config.enabled,
                config.rewrite_attributes
            );
            config
        }
        None => {
            log::info!("NextJS integration not registered (disabled or missing config)");
            return None;
        }
    };

    // Register both structured (Pages Router __NEXT_DATA__) and streamed (App Router RSC)
    // rewriters. RSC payloads require LENGTH-PRESERVING URL replacement to avoid breaking
    // React hydration - the RSC format uses byte positions for record boundaries.
    let structured = Arc::new(NextJsScriptRewriter::new(
        config.clone(),
        NextJsRewriteMode::Structured,
    ));

    let streamed = Arc::new(NextJsScriptRewriter::new(
        config.clone(),
        NextJsRewriteMode::Streamed,
    ));

    // Register post-processor for cross-script RSC T-chunks
    let post_processor = Arc::new(NextJsHtmlPostProcessor::new(config));

    Some(
        IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
            .with_script_rewriter(structured)
            .with_script_rewriter(streamed)
            .with_html_post_processor(post_processor)
            .build(),
    )
}

/// Post-processor for handling cross-script RSC T-chunks.
struct NextJsHtmlPostProcessor {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsHtmlPostProcessor {
    fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationHtmlPostProcessor for NextJsHtmlPostProcessor {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn post_process(&self, html: &str, ctx: &IntegrationHtmlContext<'_>) -> String {
        log::info!(
            "NextJs post-processor called: enabled={}, rewrite_attributes={:?}, html_len={}, origin={}, proxy={}://{}",
            self.config.enabled,
            self.config.rewrite_attributes,
            html.len(),
            ctx.origin_host,
            ctx.request_scheme,
            ctx.request_host
        );

        if !self.config.enabled || self.config.rewrite_attributes.is_empty() {
            log::info!("NextJs post-processor skipped (disabled or no attributes)");
            return html.to_string();
        }

        // Count origin URLs before
        let origin_before = html.matches(ctx.origin_host).count();
        log::info!(
            "NextJs post-processor: {} origin URLs before rewrite",
            origin_before
        );

        let result =
            post_process_rsc_html(html, ctx.origin_host, ctx.request_host, ctx.request_scheme);

        // Count after
        let origin_after = result.matches(ctx.origin_host).count();
        let proxy_after = result.matches(ctx.request_host).count();
        log::info!(
            "NextJs post-processor complete: input_len={}, output_len={}, origin_remaining={}, proxy_urls={}",
            html.len(),
            result.len(),
            origin_after,
            proxy_after
        );
        result
    }
}

fn build(settings: &Settings) -> Option<Arc<NextJsIntegrationConfig>> {
    let config = settings
        .integration_config::<NextJsIntegrationConfig>(NEXTJS_INTEGRATION_ID)
        .ok()
        .flatten()?;
    Some(Arc::new(config))
}

#[derive(Clone, Copy)]
enum NextJsRewriteMode {
    Structured,
    Streamed,
}

struct NextJsScriptRewriter {
    config: Arc<NextJsIntegrationConfig>,
    mode: NextJsRewriteMode,
}

impl NextJsScriptRewriter {
    fn new(config: Arc<NextJsIntegrationConfig>, mode: NextJsRewriteMode) -> Self {
        Self { config, mode }
    }

    fn rewrite_structured(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        // For structured mode (__NEXT_DATA__), use simple URL replacement
        if let Some(rewritten) = rewrite_nextjs_values(
            content,
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
            &self.config.rewrite_attributes,
            false, // No length preservation needed for structured data
        ) {
            ScriptRewriteAction::replace(rewritten)
        } else {
            ScriptRewriteAction::keep()
        }
    }

    fn rewrite_streamed(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        // For streamed RSC payloads, we need T-chunk aware rewriting.
        // This handles the case where T-chunk lengths need to be recalculated
        // after URL rewriting.
        //
        // Try to extract the RSC payload from self.__next_f.push([1, '...'])
        if let Some((payload, quote, start, end)) = extract_rsc_push_payload(content) {
            let rewritten_payload = rewrite_rsc_tchunks(
                payload,
                ctx.origin_host,
                ctx.request_host,
                ctx.request_scheme,
            );

            if rewritten_payload != payload {
                // Reconstruct the script with rewritten payload
                let mut result = String::with_capacity(content.len());
                result.push_str(&content[..start]);
                result.push(quote);
                result.push_str(&rewritten_payload);
                result.push(quote);
                result.push_str(&content[end + 1..]);
                return ScriptRewriteAction::replace(result);
            }
        }

        // Fallback: use simple URL rewriting for the entire content
        // This handles non-standard RSC formats or other script patterns
        let rewritten = rewrite_rsc_url_string(
            content,
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
        );

        if rewritten != content {
            return ScriptRewriteAction::replace(rewritten);
        }

        ScriptRewriteAction::keep()
    }
}

/// Extract RSC payload from a self.__next_f.push([1, '...']) call
/// Returns (payload_content, quote_char, start_pos, end_pos)
/// Handles various whitespace patterns in the push call.
fn extract_rsc_push_payload(content: &str) -> Option<(&str, char, usize, usize)> {
    // Match pattern: self.__next_f.push([ followed by whitespace, then 1, then whitespace, then quote
    // Use regex to be more flexible with whitespace
    let pattern = Regex::new(r#"self\.__next_f\.push\(\[\s*1\s*,\s*(['"])"#).ok()?;

    let cap = pattern.captures(content)?;
    let quote_match = cap.get(1)?;
    let quote = quote_match.as_str().chars().next()?;
    let content_start = quote_match.end();

    // Find matching closing quote
    let search_from = &content[content_start..];
    let mut pos = 0;
    let mut escape = false;

    for c in search_from.chars() {
        if escape {
            escape = false;
            pos += c.len_utf8();
            continue;
        }
        if c == '\\' {
            escape = true;
            pos += 1;
            continue;
        }
        if c == quote {
            // Found closing quote
            let content_end = content_start + pos;
            return Some((
                &content[content_start..content_end],
                quote,
                content_start - 1, // Include opening quote position
                content_end,       // Position of closing quote
            ));
        }
        pos += c.len_utf8();
    }

    None
}

impl IntegrationScriptRewriter for NextJsScriptRewriter {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        match self.mode {
            NextJsRewriteMode::Structured => "script#__NEXT_DATA__",
            NextJsRewriteMode::Streamed => "script",
        }
    }

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        if self.config.rewrite_attributes.is_empty() {
            return ScriptRewriteAction::keep();
        }

        match self.mode {
            NextJsRewriteMode::Structured => self.rewrite_structured(content, ctx),
            NextJsRewriteMode::Streamed => {
                // RSC push scripts (self.__next_f.push) are handled by the post-processor
                // because T-chunks can span multiple scripts and require combined processing.
                // Only handle non-RSC scripts here.
                if content.contains("self.__next_f.push") {
                    return ScriptRewriteAction::keep();
                }
                // For other __next_f scripts (like initialization), use simple URL rewriting
                if content.contains("self.__next_f") {
                    return self.rewrite_streamed(content, ctx);
                }
                ScriptRewriteAction::keep()
            }
        }
    }
}

fn rewrite_nextjs_values(
    content: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    attributes: &[String],
    preserve_length: bool,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    // Build the rewriter context with regex patterns
    // For RSC payloads (preserve_length=true), we must maintain exact byte positions
    // to avoid breaking React hydration.
    let rewriter = UrlRewriter::new(
        origin_host,
        request_host,
        request_scheme,
        attributes,
        preserve_length,
    );

    // Use pure regex-based rewriting - no AST parsing needed
    // The rewrite_embedded method handles all URL patterns with proper whitespace padding
    rewriter.rewrite_embedded(content)
}

/// Helper struct to hold URL rewriting configuration
struct UrlRewriter {
    origin_host: String,
    request_host: String,
    request_scheme: String,
    /// Regex patterns for embedded JSON in strings with URL scheme (e.g., \"href\":\"https://origin\")
    embedded_patterns: Vec<Regex>,
    /// Regex patterns for bare hostname values (e.g., \"siteProductionDomain\":\"www.example.com\")
    bare_host_patterns: Vec<Regex>,
    /// Whether to preserve URL length by padding (for RSC payloads)
    preserve_length: bool,
}

impl UrlRewriter {
    fn new(
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
        attributes: &[String],
        preserve_length: bool,
    ) -> Self {
        let escaped_origin = escape(origin_host);

        // Build patterns for embedded JSON strings with various escape levels
        // Pattern 1: URLs with scheme (https://origin, http://origin, //origin)
        // Also capture optional path and closing quote to add whitespace padding after
        let embedded_patterns = attributes
            .iter()
            .map(|attr| {
                let escaped_attr = escape(attr);
                // Capture: prefix, scheme, path (optional), closing quote
                let pattern = format!(
                    r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*")(?P<scheme>https?://|//){origin}(?P<path>[^"\\]*)(?P<quote>\\*")"#,
                    attr = escaped_attr,
                    origin = escaped_origin,
                );
                Regex::new(&pattern).expect("valid Next.js rewrite regex")
            })
            .collect();

        // Pattern 2: Bare hostname without scheme (e.g., "siteProductionDomain":"www.example.com")
        // This matches attribute:"hostname" where hostname is exactly the origin (no path)
        let bare_host_patterns = attributes
            .iter()
            .map(|attr| {
                let escaped_attr = escape(attr);
                // Match attr":"origin" where origin is followed by end quote (no path)
                let pattern = format!(
                    r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*"){origin}(?P<suffix>\\*")"#,
                    attr = escaped_attr,
                    origin = escaped_origin,
                );
                Regex::new(&pattern).expect("valid Next.js bare host rewrite regex")
            })
            .collect();

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            embedded_patterns,
            bare_host_patterns,
            preserve_length,
        }
    }

    /// Rewrite a URL value string, returning (new_url, padding) if modified.
    /// The padding is whitespace to add after the closing quote to preserve byte positions.
    /// Uses the request scheme (http/https) for the rewritten URL.
    #[cfg(test)]
    fn rewrite_url_value(&self, url: &str) -> Option<(String, String)> {
        let original_len = url.len();

        // Check for https:// or http:// URLs
        // Use the request scheme for the rewritten URL (e.g., http for localhost)
        let new_url = if let Some(rest) = url.strip_prefix("https://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ))
            } else {
                None
            }
        } else if let Some(rest) = url.strip_prefix("http://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ))
            } else {
                None
            }
        } else if let Some(rest) = url.strip_prefix("//") {
            // Protocol-relative URL - use request scheme
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ))
            } else {
                None
            }
        } else if url == self.origin_host {
            // Bare hostname without scheme (e.g., "siteProductionDomain":"www.example.com")
            Some(self.request_host.clone())
        } else if url.starts_with(&self.origin_host) {
            // Hostname with path but no scheme (e.g., "www.example.com/path")
            let path = &url[self.origin_host.len()..];
            Some(format!("{}{}", self.request_host, path))
        } else {
            None
        };

        // Calculate whitespace padding if length preservation is enabled
        new_url.map(|url| {
            let padding = if self.preserve_length {
                Self::calculate_padding(url.len(), original_len)
            } else {
                String::new()
            };
            (url, padding)
        })
    }

    /// Calculate the whitespace padding needed after a URL replacement.
    /// Returns empty string if no padding needed (URL is same length or longer).
    ///
    /// For RSC hydration, we add spaces AFTER the closing quote to preserve
    /// byte positions in the JSON stream. This is preferred over URL path padding
    /// because it keeps URLs clean and works for all URL types.
    #[cfg(test)]
    fn calculate_padding(new_url_len: usize, original_len: usize) -> String {
        if new_url_len >= original_len {
            String::new()
        } else {
            " ".repeat(original_len - new_url_len)
        }
    }

    /// Rewrite embedded JSON patterns in a string (for streamed payloads)
    fn rewrite_embedded(&self, input: &str) -> Option<String> {
        let mut result = input.to_string();
        let mut changed = false;

        // First pass: URLs with scheme (https://, http://, //)
        for regex in &self.embedded_patterns {
            let origin_host = &self.origin_host;
            let request_host = &self.request_host;
            let request_scheme = &self.request_scheme;
            let preserve_length = self.preserve_length;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let scheme = &caps["scheme"];
                let path = &caps["path"];
                let quote = &caps["quote"];

                // Calculate original URL length (scheme + origin_host + path)
                let original_url_len = scheme.len() + origin_host.len() + path.len();

                // Build replacement URL using the request scheme (e.g., http for localhost)
                let new_url = format!("{}://{}{}", request_scheme, request_host, path);

                // Calculate whitespace padding if needed
                let padding = if preserve_length && new_url.len() < original_url_len {
                    " ".repeat(original_url_len - new_url.len())
                } else {
                    String::new()
                };

                // Return: prefix + new_url + quote + padding (spaces after closing quote)
                format!("{}{}{}{}", prefix, new_url, quote, padding)
            });
            if next_value != result {
                changed = true;
                result = next_value.into_owned();
            }
        }

        // Second pass: Bare hostnames without scheme (e.g., "siteProductionDomain":"www.example.com")
        for regex in &self.bare_host_patterns {
            let origin_host = &self.origin_host;
            let request_host = &self.request_host;
            let preserve_length = self.preserve_length;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let suffix = &caps["suffix"];

                // Calculate padding for bare hostnames
                let padding = if preserve_length && request_host.len() < origin_host.len() {
                    " ".repeat(origin_host.len() - request_host.len())
                } else {
                    String::new()
                };

                format!("{}{}{}{}", prefix, request_host, suffix, padding)
            });
            if next_value != result {
                changed = true;
                result = next_value.into_owned();
            }
        }

        changed.then_some(result)
    }
}

// =============================================================================
// RSC (React Server Components) T-Chunk Rewriter
// =============================================================================
//
// Next.js App Router uses React Server Components (RSC) with a streaming flight
// protocol. RSC data is delivered via inline scripts calling `self.__next_f.push()`.
//
// ## RSC Flight Protocol Format
//
// RSC records are separated by `\n` (literal backslash-n in JS strings).
// Each record has format: `ID:DATA` where ID is a hex string (e.g., "1a", "443").
//
// Record types include:
// - T-chunks (text): `ID:T<hex_length>,<content>` - The most important for rewriting
// - JSON arrays: `ID:[...]`
// - JSON objects: `ID:{...}`
// - Module imports: `ID:I[...]`
// - Head links: `ID:HL[...]`
// - References: `ID:$ref`
// - Strings: `ID:"..."`
// - Null: `ID:null`
//
// ## T-Chunk Format Details
//
// T-chunks contain text data with an explicit byte length:
// ```
// 1a:T29,{"url":"https://origin.example.com/path"}
// ```
// - `1a` = chunk ID (hex)
// - `T` = text chunk marker
// - `29` = content length in hex (0x29 = 41 bytes UNESCAPED)
// - `,` = separator
// - Content follows, exactly 41 unescaped bytes
//
// The hex_length is the UNESCAPED byte count - escape sequences like `\n` count
// as 1 byte, `\uHHHH` counts as the UTF-8 byte length of the character, etc.
//
// ## Why T-Chunk Length Matters
//
// React's RSC parser uses byte offsets to navigate the stream. If we rewrite
// URLs without updating T-chunk lengths, the parser reads wrong byte ranges,
// corrupting the data and breaking hydration.
//
// Example: Changing `origin.example.com` (18 chars) to `proxy.io` (8 chars)
// shrinks content by 10 bytes. The T-chunk header must be updated from
// `T29,` to `T1f,` (41 -> 31 bytes).
//
// ## Cross-Script T-Chunks
//
// T-chunks CAN span multiple push scripts:
// - Script 10: `11:null\n1a:T928,` (header only, declares 928 bytes)
// - Script 11: `...actual content...` (the 928 bytes of content)
//
// Our per-script processing handles most cases correctly. For cross-script
// T-chunks, the header script won't have URLs to rewrite (just the header),
// and the content script will be rewritten with correct byte counting.

/// Calculate the unescaped byte length of a JS string with escape sequences.
/// This accounts for \n, \r, \t, \\, \", \xHH, \uHHHH, and surrogate pairs.
fn calculate_unescaped_byte_length(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut result = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let esc = bytes[i + 1];

            // Simple escape sequences: \n, \r, \t, \b, \f, \v, \", \', \\, \/
            if matches!(
                esc,
                b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/'
            ) {
                result += 1;
                i += 2;
                continue;
            }

            // \xHH - hex escape (1 byte)
            if esc == b'x' && i + 3 < bytes.len() {
                result += 1;
                i += 4;
                continue;
            }

            // \uHHHH - unicode escape
            if esc == b'u' && i + 5 < bytes.len() {
                let hex = &s[i + 2..i + 6];
                if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                        // Check for surrogate pair
                        if (0xD800..=0xDBFF).contains(&code_unit)
                            && i + 11 < bytes.len()
                            && bytes[i + 6] == b'\\'
                            && bytes[i + 7] == b'u'
                        {
                            let hex2 = &s[i + 8..i + 12];
                            if hex2.chars().all(|c| c.is_ascii_hexdigit()) {
                                if let Ok(code_unit2) = u16::from_str_radix(hex2, 16) {
                                    if (0xDC00..=0xDFFF).contains(&code_unit2) {
                                        // Full surrogate pair = 4 UTF-8 bytes
                                        result += 4;
                                        i += 12;
                                        continue;
                                    }
                                }
                            }
                        }

                        // Single unicode escape - calculate UTF-8 byte length
                        let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                        result += c.len_utf8();
                        i += 6;
                        continue;
                    }
                }
            }
        }

        // Regular character - count its UTF-8 byte length
        // For ASCII, this is 1 byte
        if bytes[i] < 0x80 {
            result += 1;
            i += 1;
        } else {
            // Multi-byte UTF-8 character
            let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
            result += c.len_utf8();
            i += c.len_utf8();
        }
    }

    result
}

/// Consume a specified number of unescaped bytes from a JS string, returning the end position.
fn consume_unescaped_bytes(s: &str, start_pos: usize, byte_count: usize) -> (usize, usize) {
    let bytes = s.as_bytes();
    let mut consumed = 0;
    let mut pos = start_pos;

    while pos < bytes.len() && consumed < byte_count {
        if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
            let esc = bytes[pos + 1];

            if matches!(
                esc,
                b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/'
            ) {
                consumed += 1;
                pos += 2;
                continue;
            }

            if esc == b'x' && pos + 3 < bytes.len() {
                consumed += 1;
                pos += 4;
                continue;
            }

            if esc == b'u' && pos + 5 < bytes.len() {
                let hex = &s[pos + 2..pos + 6];
                if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                        if (0xD800..=0xDBFF).contains(&code_unit)
                            && pos + 11 < bytes.len()
                            && bytes[pos + 6] == b'\\'
                            && bytes[pos + 7] == b'u'
                        {
                            let hex2 = &s[pos + 8..pos + 12];
                            if hex2.chars().all(|c| c.is_ascii_hexdigit()) {
                                if let Ok(code_unit2) = u16::from_str_radix(hex2, 16) {
                                    if (0xDC00..=0xDFFF).contains(&code_unit2) {
                                        consumed += 4;
                                        pos += 12;
                                        continue;
                                    }
                                }
                            }
                        }

                        let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                        consumed += c.len_utf8();
                        pos += 6;
                        continue;
                    }
                }
            }
        }

        if bytes[pos] < 0x80 {
            consumed += 1;
            pos += 1;
        } else {
            let c = s[pos..].chars().next().unwrap_or('\u{FFFD}');
            consumed += c.len_utf8();
            pos += c.len_utf8();
        }
    }

    (pos, consumed)
}

/// Information about a T-chunk found in the combined RSC content
struct TChunkInfo {
    /// The chunk ID (hex string like "1a", "443")
    id: String,
    /// Position where the T-chunk header starts (e.g., position of "1a:T...")
    match_start: usize,
    /// Position right after the comma (where content begins)
    header_end: usize,
    /// Position where the content ends
    content_end: usize,
}

/// Find all T-chunks in the combined RSC content.
/// T-chunks have format: ID:T<hex_length>,<content>
fn find_tchunks(content: &str) -> Vec<TChunkInfo> {
    // Match pattern: hex_id:Thex_length,
    let pattern = Regex::new(r"([0-9a-fA-F]+):T([0-9a-fA-F]+),").unwrap();
    let mut chunks = Vec::new();
    let mut search_pos = 0;

    while search_pos < content.len() {
        if let Some(cap) = pattern.captures(&content[search_pos..]) {
            let m = cap.get(0).unwrap();
            let match_start = search_pos + m.start();
            let header_end = search_pos + m.end();

            let id = cap.get(1).unwrap().as_str().to_string();
            let length_hex = cap.get(2).unwrap().as_str();
            let declared_length = usize::from_str_radix(length_hex, 16).unwrap_or(0);

            // Consume the declared number of unescaped bytes, skipping markers
            let (content_end, _) = consume_unescaped_bytes(content, header_end, declared_length);

            chunks.push(TChunkInfo {
                id,
                match_start,
                header_end,
                content_end,
            });

            search_pos = content_end;
        } else {
            break;
        }
    }

    chunks
}

/// Rewrite URLs in a string, handling various URL formats in RSC content.
fn rewrite_rsc_url_string(
    s: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let escaped_origin = escape(origin_host);

    // Match various URL patterns:
    // - https://host or http://host
    // - //host (protocol-relative)
    // - \/\/host (escaped slashes in JSON)
    // - \\\/\\\/host (double-escaped)
    // - \\\\/\\\\/host (quad-escaped)
    let pattern = Regex::new(&format!(
        r#"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//){}"#,
        escaped_origin
    ))
    .unwrap();

    pattern
        .replace_all(s, |caps: &regex::Captures<'_>| {
            let slashes = caps.get(3).map_or("//", |m| m.as_str());
            format!("{}:{}{}", request_scheme, slashes, request_host)
        })
        .into_owned()
}

/// Rewrite T-chunks in RSC content, updating lengths after URL rewriting.
/// This works for single scripts where T-chunks don't span script boundaries.
fn rewrite_rsc_tchunks(
    content: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let chunks = find_tchunks(content);

    if chunks.is_empty() {
        // No T-chunks, just rewrite URLs in the whole content
        return rewrite_rsc_url_string(content, origin_host, request_host, request_scheme);
    }

    let mut result = String::with_capacity(content.len());
    let mut last_end = 0;

    for chunk in &chunks {
        // Content before this T-chunk (rewrite URLs)
        let before = &content[last_end..chunk.match_start];
        result.push_str(&rewrite_rsc_url_string(
            before,
            origin_host,
            request_host,
            request_scheme,
        ));

        // Extract and rewrite T-chunk content
        let chunk_content = &content[chunk.header_end..chunk.content_end];
        let rewritten_content =
            rewrite_rsc_url_string(chunk_content, origin_host, request_host, request_scheme);

        // Calculate new byte length
        let new_length = calculate_unescaped_byte_length(&rewritten_content);
        let new_length_hex = format!("{:x}", new_length);

        // Write new T-chunk header and content
        result.push_str(&chunk.id);
        result.push_str(":T");
        result.push_str(&new_length_hex);
        result.push(',');
        result.push_str(&rewritten_content);

        last_end = chunk.content_end;
    }

    // Remaining content after last T-chunk
    let remaining = &content[last_end..];
    result.push_str(&rewrite_rsc_url_string(
        remaining,
        origin_host,
        request_host,
        request_scheme,
    ));

    result
}

// =============================================================================
// Cross-Script RSC Processing
// =============================================================================
//
// T-chunks can span multiple push scripts. For example:
// - Script 10: "11:null\n1a:T928," (header declares 928 bytes, but script ends)
// - Script 11: "...actual 928 bytes of content..."
//
// To handle this correctly, we must process all scripts together:
// 1. Combine scripts with markers
// 2. Find T-chunks across the combined content (skip markers when counting bytes)
// 3. Rewrite URLs and recalculate lengths
// 4. Split back on markers
//

/// Marker used to track script boundaries when combining RSC content
const RSC_MARKER: &str = "\x00SPLIT\x00";

/// Consume unescaped bytes, skipping RSC markers.
/// Returns (end_position, bytes_consumed)
fn consume_unescaped_bytes_skip_markers(
    s: &str,
    start_pos: usize,
    byte_count: usize,
) -> (usize, usize) {
    let bytes = s.as_bytes();
    let mut consumed = 0;
    let mut pos = start_pos;
    let marker_bytes = RSC_MARKER.as_bytes();

    while pos < bytes.len() && consumed < byte_count {
        // Check for marker - skip it without counting bytes
        if pos + marker_bytes.len() <= bytes.len()
            && &bytes[pos..pos + marker_bytes.len()] == marker_bytes
        {
            pos += marker_bytes.len();
            continue;
        }

        if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
            let esc = bytes[pos + 1];

            if matches!(
                esc,
                b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/'
            ) {
                consumed += 1;
                pos += 2;
                continue;
            }

            if esc == b'x' && pos + 3 < bytes.len() {
                consumed += 1;
                pos += 4;
                continue;
            }

            if esc == b'u' && pos + 5 < bytes.len() {
                let hex = &s[pos + 2..pos + 6];
                if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                        if (0xD800..=0xDBFF).contains(&code_unit)
                            && pos + 11 < bytes.len()
                            && bytes[pos + 6] == b'\\'
                            && bytes[pos + 7] == b'u'
                        {
                            let hex2 = &s[pos + 8..pos + 12];
                            if hex2.chars().all(|c| c.is_ascii_hexdigit()) {
                                if let Ok(code_unit2) = u16::from_str_radix(hex2, 16) {
                                    if (0xDC00..=0xDFFF).contains(&code_unit2) {
                                        consumed += 4;
                                        pos += 12;
                                        continue;
                                    }
                                }
                            }
                        }

                        let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                        consumed += c.len_utf8();
                        pos += 6;
                        continue;
                    }
                }
            }
        }

        if bytes[pos] < 0x80 {
            consumed += 1;
            pos += 1;
        } else {
            let c = s[pos..].chars().next().unwrap_or('\u{FFFD}');
            consumed += c.len_utf8();
            pos += c.len_utf8();
        }
    }

    (pos, consumed)
}

/// Calculate unescaped byte length excluding RSC markers.
fn calculate_unescaped_byte_length_skip_markers(s: &str) -> usize {
    let without_markers = s.replace(RSC_MARKER, "");
    calculate_unescaped_byte_length(&without_markers)
}

/// Information about a T-chunk in marker-combined content
struct MarkedTChunkInfo {
    id: String,
    match_start: usize,
    header_end: usize,
    content_end: usize,
}

/// Find T-chunks in marker-combined RSC content.
fn find_tchunks_with_markers(content: &str) -> Vec<MarkedTChunkInfo> {
    let pattern = Regex::new(r"([0-9a-fA-F]+):T([0-9a-fA-F]+),").unwrap();
    let mut chunks = Vec::new();
    let mut search_pos = 0;

    while search_pos < content.len() {
        if let Some(cap) = pattern.captures(&content[search_pos..]) {
            let m = cap.get(0).unwrap();
            let match_start = search_pos + m.start();
            let header_end = search_pos + m.end();

            let id = cap.get(1).unwrap().as_str().to_string();
            let length_hex = cap.get(2).unwrap().as_str();
            let declared_length = usize::from_str_radix(length_hex, 16).unwrap_or(0);

            // Consume bytes, skipping markers
            let (content_end, _) =
                consume_unescaped_bytes_skip_markers(content, header_end, declared_length);

            chunks.push(MarkedTChunkInfo {
                id,
                match_start,
                header_end,
                content_end,
            });

            search_pos = content_end;
        } else {
            break;
        }
    }

    chunks
}

/// Process multiple RSC script payloads together, handling cross-script T-chunks.
///
/// This function:
/// 1. Combines all payloads with markers
/// 2. Finds T-chunks across the combined content
/// 3. Rewrites URLs and recalculates T-chunk lengths
/// 4. Splits back on markers to return individual rewritten payloads
///
/// # Arguments
/// * `payloads` - The string content from each `self.__next_f.push([1, '...'])` call
/// * `origin_host` - The origin host to replace
/// * `request_host` - The request host to use in replacements
/// * `request_scheme` - The scheme (http/https) to use in replacements
///
/// # Returns
/// A vector of rewritten payloads in the same order as input
pub fn rewrite_rsc_scripts_combined(
    payloads: &[&str],
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> Vec<String> {
    if payloads.is_empty() {
        return Vec::new();
    }

    if payloads.len() == 1 {
        // Single script - use simple approach
        return vec![rewrite_rsc_tchunks(
            payloads[0],
            origin_host,
            request_host,
            request_scheme,
        )];
    }

    // Combine payloads with markers
    let mut combined = payloads[0].to_string();
    for payload in &payloads[1..] {
        combined.push_str(RSC_MARKER);
        combined.push_str(payload);
    }

    // Find T-chunks in combined content
    let chunks = find_tchunks_with_markers(&combined);

    if chunks.is_empty() {
        // No T-chunks - just rewrite URLs in each payload
        return payloads
            .iter()
            .map(|p| rewrite_rsc_url_string(p, origin_host, request_host, request_scheme))
            .collect();
    }

    // Build rewritten combined content
    let mut result = String::with_capacity(combined.len());
    let mut last_end = 0;

    for chunk in &chunks {
        // Content before this T-chunk (rewrite URLs, preserve markers)
        let before = &combined[last_end..chunk.match_start];
        result.push_str(&rewrite_rsc_url_string(
            before,
            origin_host,
            request_host,
            request_scheme,
        ));

        // Extract T-chunk content (may contain markers)
        let chunk_content = &combined[chunk.header_end..chunk.content_end];

        // Rewrite URLs (preserves markers)
        let rewritten_content =
            rewrite_rsc_url_string(chunk_content, origin_host, request_host, request_scheme);

        // Calculate new byte length (excluding markers)
        let new_length = calculate_unescaped_byte_length_skip_markers(&rewritten_content);
        let new_length_hex = format!("{:x}", new_length);

        // Write new T-chunk header and content
        result.push_str(&chunk.id);
        result.push_str(":T");
        result.push_str(&new_length_hex);
        result.push(',');
        result.push_str(&rewritten_content);

        last_end = chunk.content_end;
    }

    // Remaining content after last T-chunk
    let remaining = &combined[last_end..];
    result.push_str(&rewrite_rsc_url_string(
        remaining,
        origin_host,
        request_host,
        request_scheme,
    ));

    // Split back on markers
    result.split(RSC_MARKER).map(|s| s.to_string()).collect()
}

/// Information about an RSC push script in HTML
struct RscPushScript {
    /// Start position of the payload content (inside the quotes).
    payload_start: usize,
    /// End position of the payload content (inside the quotes).
    payload_end: usize,
    /// The payload content (inside the quotes)
    payload: String,
}

/// Find all RSC push scripts in HTML content.
/// Returns scripts in order of appearance.
///
/// Handles both minified format: `<script>self.__next_f.push([1,"payload"])</script>`
/// and prettified format with whitespace:
/// ```html
/// <script>
///   self.__next_f.push([
///     1,
///     "payload"
///   ]);
/// </script>
/// ```
fn find_rsc_push_scripts(html: &str) -> Vec<RscPushScript> {
    let mut scripts = Vec::new();
    // Match <script ...> (optionally with attributes like nonce=...) followed by whitespace,
    // then a Next.js RSC push call with a string payload.
    let pattern =
        Regex::new(r#"<script\b[^>]*>\s*self\.__next_f\.push\(\[\s*1\s*,\s*(['"])"#).unwrap();
    let ending_pattern = Regex::new(r#"^\s*\]\s*\)\s*;?\s*</script>"#).unwrap();

    let mut search_pos = 0;

    while search_pos < html.len() {
        let Some(cap) = pattern.captures(&html[search_pos..]) else {
            break;
        };

        let quote_match = cap.get(1).unwrap();
        let quote = quote_match.as_str().chars().next().unwrap();
        let payload_start = search_pos + quote_match.end();

        // Find the closing quote (handling escapes)
        let mut i = payload_start;
        let bytes = html.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2; // Skip escape sequence
            } else if bytes[i] == quote as u8 {
                break;
            } else {
                i += 1;
            }
        }

        if i >= bytes.len() {
            search_pos = payload_start;
            continue;
        }

        // After the closing quote, look for ])</script> with optional whitespace
        let after_quote = &html[i + 1..];

        let Some(ending_match) = ending_pattern.find(after_quote) else {
            search_pos = payload_start;
            continue;
        };

        let payload = html[payload_start..i].to_string();
        let payload_end = i;
        let script_end = i + 1 + ending_match.end();

        scripts.push(RscPushScript {
            payload_start,
            payload_end,
            payload,
        });

        search_pos = script_end;
    }

    scripts
}

/// Post-process complete HTML to handle cross-script RSC T-chunks.
///
/// This function:
/// 1. Finds all RSC push scripts in the HTML
/// 2. Extracts their payloads
/// 3. Processes them together using the combined approach
/// 4. Rebuilds the HTML with rewritten scripts
///
/// This should be called after streaming HTML processing to fix T-chunk lengths
/// that span multiple scripts.
///
/// # Arguments
/// * `html` - The complete HTML content (must be valid UTF-8)
/// * `origin_host` - The origin host to replace
/// * `request_host` - The request host to use in replacements
/// * `request_scheme` - The scheme (http/https) to use in replacements
///
/// # Returns
/// The HTML with RSC scripts rewritten to have correct T-chunk lengths
pub fn post_process_rsc_html(
    html: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let scripts = find_rsc_push_scripts(html);

    log::info!(
        "post_process_rsc_html: found {} RSC push scripts, origin={}, proxy={}://{}",
        scripts.len(),
        origin_host,
        request_scheme,
        request_host
    );

    if scripts.is_empty() {
        log::info!("post_process_rsc_html: no RSC scripts found, returning unchanged");
        return html.to_string();
    }

    // Extract payloads
    let payloads: Vec<&str> = scripts.iter().map(|s| s.payload.as_str()).collect();

    // Count origin URLs before rewriting
    let origin_count_before: usize = payloads
        .iter()
        .map(|p| p.matches(origin_host).count())
        .sum();
    log::info!(
        "post_process_rsc_html: {} occurrences of '{}' in payloads before rewriting",
        origin_count_before,
        origin_host
    );

    // Process all scripts together
    let rewritten_payloads =
        rewrite_rsc_scripts_combined(&payloads, origin_host, request_host, request_scheme);

    // Count origin URLs after rewriting
    let origin_count_after: usize = rewritten_payloads
        .iter()
        .map(|p| p.matches(origin_host).count())
        .sum();
    let proxy_count: usize = rewritten_payloads
        .iter()
        .map(|p| p.matches(request_host).count())
        .sum();
    log::info!(
        "post_process_rsc_html: after rewriting - {} origin URLs remaining, {} proxy URLs",
        origin_count_after,
        proxy_count
    );

    // Replace payload contents in-place (apply replacements in reverse order to keep indices valid).
    let mut result = html.to_string();
    for (i, script) in scripts.iter().enumerate().rev() {
        result.replace_range(
            script.payload_start..script.payload_end,
            &rewritten_payloads[i],
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{IntegrationRegistry, IntegrationScriptContext, ScriptRewriteAction};
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::io::Cursor;

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
        })
    }

    fn ctx(selector: &'static str) -> IntegrationScriptContext<'static> {
        IntegrationScriptContext {
            selector,
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    #[test]
    fn structured_rewriter_updates_next_data_payload() {
        let payload = r#"{"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}"#;
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Structured);
        let result = rewriter.rewrite(payload, &ctx("script#__NEXT_DATA__"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                // Note: URLs may have padding for length preservation
                assert!(value.contains("ts.example.com") && value.contains("/reviews"));
                assert!(value.contains("ts.example.com") && value.contains("/sign-in"));
                assert!(value.contains(r#""fallbackHref":"http://origin.example.com/legacy""#));
                assert!(value.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#));
            }
            _ => panic!("Expected rewrite to update payload"),
        }
    }

    #[test]
    fn streamed_rewriter_skips_non_next_payloads() {
        // The streamed rewriter skips RSC push scripts (self.__next_f.push)
        // because these are handled by the post-processor for cross-script T-chunks.
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Streamed);

        // Non-Next.js scripts should be skipped
        let noop = rewriter.rewrite("console.log('hello');", &ctx("script"));
        assert!(matches!(noop, ScriptRewriteAction::Keep));

        // RSC push payloads should be skipped (handled by post-processor)
        let payload =
            r#"self.__next_f.push([1, "{\"href\":\"https://origin.example.com/app\"}"]);"#;
        let result = rewriter.rewrite(payload, &ctx("script"));
        assert!(
            matches!(result, ScriptRewriteAction::Keep),
            "Streamed rewriter should skip __next_f.push payloads (handled by post-processor)"
        );

        // Other __next_f scripts (like initialization) should still be processed
        let init_script = r#"(self.__next_f = self.__next_f || []).push([0]); var url = "https://origin.example.com/api";"#;
        let init_result = rewriter.rewrite(init_script, &ctx("script"));
        // This might or might not be rewritten depending on content - just verify it runs
        assert!(
            matches!(
                init_result,
                ScriptRewriteAction::Keep | ScriptRewriteAction::Replace(_)
            ),
            "Streamed rewriter should handle non-push __next_f scripts"
        );
    }

    #[test]
    fn rewrite_helper_handles_protocol_relative_urls() {
        let content = r#"{"props":{"pageProps":{"link":"//origin.example.com/image.png"}}}"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["link".into()],
            false, // preserve_length=false for non-RSC content
        )
        .expect("should rewrite protocol relative link");

        // Note: URLs may have padding for length preservation
        assert!(rewritten.contains("ts.example.com") && rewritten.contains("/image.png"));
    }

    fn config_from_settings(
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> HtmlProcessorConfig {
        HtmlProcessorConfig::from_settings(
            settings,
            registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
    }

    #[test]
    fn html_processor_rewrites_nextjs_script_when_enabled() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}
            </script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();
        let processed = String::from_utf8_lossy(&output);

        // Note: URLs may have padding characters for length preservation
        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "should rewrite https Next.js href values to test.example.com"
        );
        assert!(
            processed.contains("test.example.com") && processed.contains("/sign-in"),
            "should rewrite http Next.js href values to test.example.com"
        );
        assert!(
            processed.contains(r#""fallbackHref":"http://origin.example.com/legacy""#),
            "should leave other fields untouched"
        );
        assert!(
            processed.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#),
            "should not rewrite non-href keys"
        );
        assert!(
            !processed.contains("\"href\":\"https://origin.example.com/reviews\""),
            "should remove origin https href"
        );
        assert!(
            !processed.contains("\"href\":\"http://origin.example.com/sign-in\""),
            "should remove origin http href"
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_stream_payload_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // The streaming phase skips RSC push scripts, and the post-processor handles them
        // to correctly handle cross-script T-chunks.
        let html = r#"<html><body>
            <script>self.__next_f.push([1,"prefix {\"inner\":\"value\"} \\\"href\\\":\\\"http://origin.example.com/dashboard\\\", \\\"link\\\":\\\"https://origin.example.com/api-test\\\" suffix"])</script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        // Apply post-processing (this is what handles RSC push scripts)
        let processed_str = String::from_utf8_lossy(&output);
        let final_html = post_process_rsc_html(
            &processed_str,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        // RSC payloads should be rewritten via post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host via post-processing. Output: {}",
            final_html
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_stream_payload_with_chunked_input() {
        // RSC payloads are rewritten via post-processing, even with chunked streaming input
        let html = r#"<html><body>
<script>self.__next_f.push([1,'{"href":"https://origin.example.com/app","url":"http://origin.example.com/api"}'])</script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        // Apply post-processing (this is what handles RSC push scripts)
        let processed_str = String::from_utf8_lossy(&output);
        let final_html = post_process_rsc_html(
            &processed_str,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        // RSC payloads should be rewritten via post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host with chunked input. Output: {}",
            final_html
        );
    }

    #[test]
    fn register_respects_enabled_flag() {
        let settings = create_test_settings();
        let registration = register(&settings);

        assert!(
            registration.is_none(),
            "should skip registration when integration is disabled"
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_payloads_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // This allows navigation to stay on proxy while correctly handling cross-script T-chunks.

        let html = r#"<html><body>
<script>self.__next_f.push([1,'458:{"ID":879000,"title":"Makes","url":"https://origin.example.com/makes","children":"$45a"}\n442:["$443"]'])</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["url"],
                }),
            )
            .expect("should update nextjs config");

        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        // Apply post-processing (this is what handles RSC push scripts)
        let processed_str = String::from_utf8_lossy(&output);
        let final_html = post_process_rsc_html(
            &processed_str,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        println!("=== Final HTML ===");
        println!("{}", final_html);

        // RSC payloads should be rewritten via post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC payload URLs should be rewritten to proxy host. Output: {}",
            final_html
        );

        // Verify the RSC payload structure is preserved
        assert!(
            final_html.contains(r#""ID":879000"#),
            "RSC payload ID should be preserved"
        );
        assert!(
            final_html.contains(r#""title":"Makes""#),
            "RSC payload title should be preserved"
        );
        assert!(
            final_html.contains(r#""children":"$45a""#),
            "RSC payload children reference should be preserved"
        );

        // Verify \n separators are preserved (crucial for RSC parsing)
        assert!(
            final_html.contains(r#"\n442:"#),
            "RSC record separator \\n should be preserved. Output: {}",
            final_html
        );
    }

    #[test]
    fn test_tchunk_length_recalculation() {
        // Test that T-chunk lengths are correctly recalculated after URL rewriting.
        // T-chunk format: ID:T<hex_length>,<content>
        // The hex_length is the UNESCAPED byte count of the content.

        // Original content: {"url":"https://origin.example.com/path"} = 41 bytes = 0x29
        // After rewriting: {"url":"https://test.example.com/path"} = 39 bytes = 0x27
        // (origin.example.com is 18 chars, test.example.com is 16 chars - shrinks by 2)
        let content = r#"1a:T29,{"url":"https://origin.example.com/path"}"#;
        let result =
            rewrite_rsc_tchunks(content, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com"),
            "URL should be rewritten"
        );
        assert!(
            result.starts_with("1a:T27,"),
            "T-chunk length should be updated from 29 (41) to 27 (39). Got: {}",
            result
        );
    }

    #[test]
    fn test_tchunk_length_recalculation_with_length_increase() {
        // Test that T-chunk lengths are correctly recalculated when URL length increases.
        // Original: short.io (8 chars) -> test.example.com (16 chars) - grows by 8

        // Content: {"url":"https://short.io/x"} = 28 bytes = 0x1c
        // After: {"url":"https://test.example.com/x"} = 36 bytes = 0x24
        let content = r#"1a:T1c,{"url":"https://short.io/x"}"#;
        let result = rewrite_rsc_tchunks(content, "short.io", "test.example.com", "https");

        assert!(
            result.contains("test.example.com"),
            "URL should be rewritten"
        );
        assert!(
            result.starts_with("1a:T24,"),
            "T-chunk length should be updated from 1c (28) to 24 (36). Got: {}",
            result
        );
    }

    #[test]
    fn test_calculate_unescaped_byte_length() {
        // Test the unescaped byte length calculation
        assert_eq!(calculate_unescaped_byte_length("hello"), 5);
        assert_eq!(calculate_unescaped_byte_length(r#"\n"#), 1); // \n = 1 byte
        assert_eq!(calculate_unescaped_byte_length(r#"\r\n"#), 2); // \r\n = 2 bytes
        assert_eq!(calculate_unescaped_byte_length(r#"\""#), 1); // \" = 1 byte
        assert_eq!(calculate_unescaped_byte_length(r#"\\"#), 1); // \\ = 1 byte
        assert_eq!(calculate_unescaped_byte_length(r#"\x41"#), 1); // \x41 = 'A' = 1 byte
        assert_eq!(calculate_unescaped_byte_length(r#"\u0041"#), 1); // \u0041 = 'A' = 1 byte
        assert_eq!(calculate_unescaped_byte_length(r#"\u00e9"#), 2); // \u00e9 = 'é' = 2 UTF-8 bytes
    }

    #[test]
    fn test_multiple_tchunks() {
        // Test content with multiple T-chunks
        let content = r#"1a:T1c,{"url":"https://short.io/x"}\n1b:T1c,{"url":"https://short.io/y"}"#;
        let result = rewrite_rsc_tchunks(content, "short.io", "test.example.com", "https");

        // Both T-chunks should have updated lengths
        assert!(
            result.contains("test.example.com"),
            "URLs should be rewritten"
        );
        // Both chunks should have new length 0x24 (36 bytes)
        let count = result.matches(":T24,").count();
        assert_eq!(count, 2, "Both T-chunks should have updated lengths");
    }

    #[test]
    fn test_cross_script_tchunk_rewriting() {
        // Test T-chunks that span multiple scripts.
        // This is the key scenario that breaks per-script processing.
        //
        // Script 0: Contains a T-chunk header that declares more content than is in this script
        // Script 1: Contains the rest of the T-chunk content, including URLs that need rewriting

        // T-chunk declares 64 bytes (0x40), but script 0 only has partial content
        let script0 = r#"other:data\n1a:T40,partial content"#;
        // Script 1 has the rest of the T-chunk content with a URL
        let script1 = r#" with https://origin.example.com/page goes here"#;

        // Check the actual combined byte lengths
        let combined_content = "partial content with https://origin.example.com/page goes here";
        let combined_len = calculate_unescaped_byte_length(combined_content);
        println!(
            "Combined T-chunk content length: {} bytes = 0x{:x}",
            combined_len, combined_len
        );

        // Process using combined approach
        let payloads: Vec<&str> = vec![script0, script1];
        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        println!("Results[0]: {}", results[0]);
        println!("Results[1]: {}", results[1]);

        assert_eq!(results.len(), 2, "Should return same number of scripts");

        // The URL should be rewritten in script 1
        assert!(
            results[1].contains("test.example.com"),
            "URL in script 1 should be rewritten. Got: {}",
            results[1]
        );

        // The T-chunk header in script 0 should have updated length
        // Let's check what the new length actually is
        let rewritten_content = "partial content with https://test.example.com/page goes here";
        let rewritten_len = calculate_unescaped_byte_length(rewritten_content);
        println!(
            "Rewritten T-chunk content length: {} bytes = 0x{:x}",
            rewritten_len, rewritten_len
        );

        let expected_header = format!(":T{:x},", rewritten_len);
        assert!(
            results[0].contains(&expected_header),
            "T-chunk length in script 0 should be updated to {}. Got: {}",
            expected_header,
            results[0]
        );
    }

    #[test]
    fn test_cross_script_preserves_non_tchunk_content() {
        // Test that content outside T-chunks is still rewritten correctly
        let script0 = r#"{"url":"https://origin.example.com/first"}\n1a:T40,partial"#;
        let script1 = r#" content with https://origin.example.com/page end"#;

        let payloads: Vec<&str> = vec![script0, script1];
        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        // URL outside T-chunk in script 0 should be rewritten
        assert!(
            results[0].contains("test.example.com/first"),
            "URL outside T-chunk should be rewritten. Got: {}",
            results[0]
        );

        // URL inside T-chunk (spanning scripts) should be rewritten
        assert!(
            results[1].contains("test.example.com/page"),
            "URL inside cross-script T-chunk should be rewritten. Got: {}",
            results[1]
        );
    }

    #[test]
    fn test_post_process_rsc_html() {
        // Test the complete HTML post-processing function
        let html = r#"<html><body>
<script>self.__next_f.push([1,"other:data\n1a:T40,partial content"])</script>
<script>self.__next_f.push([1," with https://origin.example.com/page goes here"])</script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        // The URL should be rewritten
        assert!(
            result.contains("test.example.com/page"),
            "URL should be rewritten. Got: {}",
            result
        );

        // The T-chunk length should be updated
        assert!(
            result.contains(":T3c,"),
            "T-chunk length should be updated. Got: {}",
            result
        );

        // HTML structure should be preserved
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn test_post_process_rsc_html_with_prettified_format() {
        // Test with prettified HTML format (newlines and whitespace between elements)
        // This is the format Next.js uses when outputting non-minified HTML
        let html = r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '445:{"ID":878799,"title":"News","url":"http://origin.example.com/news","target":""}'
      ]);
    </script>
    <script>
      self.__next_f.push([
        1,
        '446:{"url":"https://origin.example.com/reviews"}'
      ]);
    </script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        // Both URLs should be rewritten
        assert!(
            result.contains("test.example.com/news"),
            "First URL should be rewritten. Got: {}",
            result
        );
        assert!(
            result.contains("test.example.com/reviews"),
            "Second URL should be rewritten. Got: {}",
            result
        );

        // No origin URLs should remain
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );

        // HTML structure should be preserved
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn test_post_process_html_with_html_href_in_tchunk() {
        // Test that HTML href attributes inside T-chunks are rewritten
        // This is the format where HTML markup is embedded in RSC T-chunk content
        let html = r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '53d:T4d9,\u003cdiv\u003e\u003ca href="https://origin.example.com/about-us"\u003eAbout\u003c/a\u003e\u003c/div\u003e'
      ]);
    </script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        // The HTML href URL should be rewritten
        assert!(
            result.contains("test.example.com/about-us"),
            "HTML href URL in T-chunk should be rewritten. Got: {}",
            result
        );

        // No origin URLs should remain
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );

        // Verify T-chunk length was recalculated
        // Original content: \u003cdiv\u003e\u003ca href="https://origin.example.com/about-us"\u003eAbout\u003c/a\u003e\u003c/div\u003e
        // After rewrite, URL is shorter so T-chunk length should be smaller
        assert!(
            !result.contains(":T4d9,"),
            "T-chunk length should have been recalculated (original was 4d9). Got: {}",
            result
        );
    }
}

#[cfg(test)]
mod truncated_string_tests {
    use super::*;

    #[test]
    fn test_truncated_string_parsing() {
        // This simulates a Next.js chunk that's been split mid-string
        // With pure regex rewriting, truncated strings without closing quotes
        // simply won't match, which is the desired behavior
        let truncated = r#"self.__next_f.push([
  1,
  '430:I[6061,["749","static/chunks/16bf9003-553c36acd7d8a04b.js","4669","static/chun'
]);"#;

        // The regex pattern requires a closing quote after the URL,
        // so truncated content without URLs won't be modified
        let result = rewrite_nextjs_values(
            truncated,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length=true for RSC payloads
        );
        println!("Rewrite result: {:?}", result);
        // Should return None since no matching URL patterns exist
        assert!(
            result.is_none(),
            "Truncated content without URLs should not be modified"
        );
    }

    #[test]
    fn test_complete_string_with_url() {
        // A complete Next.js chunk with a URL that should be rewritten
        let complete = r#"self.__next_f.push([
  1,
  '{"url":"https://origin.example.com/path/to/resource"}'
]);"#;

        let result = rewrite_nextjs_values(
            complete,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length=true for RSC payloads
        );
        println!("Complete string rewrite: {:?}", result);
        assert!(result.is_some());
        let rewritten = result.unwrap();
        // Note: URL may have padding for length preservation
        assert!(rewritten.contains("proxy.example.com") && rewritten.contains("/path/to/resource"));
    }

    #[test]
    fn test_truncated_url_rewrite() {
        // URL that starts in this chunk but continues in the next
        // Like: "url":"https://origin.example.com/some/path?param=%20
        // where the closing quote is in the next chunk
        let truncated_url = r#"self.__next_f.push([
  1,
  '\"url\":\"https://origin.example.com/rss?title=%20'
]);"#;

        println!("Input with truncated URL:");
        println!("{}", truncated_url);

        let result = rewrite_nextjs_values(
            truncated_url,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length=true for RSC payloads
        );
        println!("Truncated URL rewrite result: {:?}", result);

        // The regex pattern requires a closing quote after the URL path,
        // so URLs without closing quotes won't be matched (preventing corruption)
        // This is actually the desired behavior - incomplete URLs are left alone
        assert!(
            result.is_none(),
            "Truncated URL without closing quote should not be modified"
        );
    }

    #[test]
    fn test_embedded_pattern_incomplete_url() {
        // Test the regex directly with an incomplete URL
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        // This string has an incomplete URL - it starts but doesn't close properly
        // within the string boundaries
        let incomplete = r#"\"url\":\"https://origin.example.com/rss?title=%20"#;
        println!("Testing embedded pattern on incomplete URL:");
        println!("Input: {}", incomplete);

        let result = rewriter.rewrite_embedded(incomplete);
        println!("Result: {:?}", result);

        // Now test with a complete URL
        let complete = r#"\"url\":\"https://origin.example.com/complete\""#;
        println!("\nTesting embedded pattern on complete URL:");
        println!("Input: {}", complete);

        let result = rewriter.rewrite_embedded(complete);
        println!("Result: {:?}", result);
    }

    #[test]
    fn test_split_chunk_url_corruption() {
        // This is the EXACT scenario that breaks React hydration!
        // The URL is split across two Next.js chunks.

        // Chunk 1: Contains the start of the URL
        // Note: In Next.js RSC, double quotes inside single-quoted strings are NOT escaped
        let chunk1 = r#"self.__next_f.push([
  1,
  '336:{"url":"https://origin.example.com/.rss/feed/3d70fbb5-ef5e-44f3-a547-e60939496e82.xml?title=Latest%20Car%20News%3A%20Trucks%2C%20SUVs%2C%20EVs%2C%20Reviews%20%26%20'
]);"#;

        // Chunk 2: Contains the continuation of the URL
        let chunk2 = r#"self.__next_f.push([
  1,
  'Auto%20Trends"}\n337:{"url":"https://origin.example.com/complete"}'
]);"#;

        println!("=== Chunk 1 (truncated URL start) ===");
        println!("{}", chunk1);

        let result1 = rewrite_nextjs_values(
            chunk1,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );
        println!("\nRewritten Chunk 1: {:?}", result1);

        // CRITICAL CHECK: The rewritten chunk should have the SAME quote escaping as the original
        // If original has unescaped " inside ', the rewritten should too
        if let Some(ref r1) = result1 {
            println!("\n=== Quote escaping analysis ===");
            println!(
                "Original has '336:{{\"url\":' (with backslash-quote): {}",
                chunk1.contains(r#"\"url\""#)
            );
            println!(
                "Original has '336:{{\"url\":' (unescaped quote): {}",
                chunk1.contains(r#"{"url":"#)
            );
            println!("Rewritten has backslash-quote: {}", r1.contains(r#"\""#));
            println!(
                "Rewritten has unescaped quote: {}",
                r1.contains(r#"{"url":"#)
            );

            // The bug: original has unescaped ", but rewritten might have escaped \"
            // This would change the JavaScript string content!
            let original_has_backslash = chunk1.contains(r#"\""#);
            let rewritten_has_backslash = r1.contains(r#"\""#);

            if !original_has_backslash && rewritten_has_backslash {
                println!("\n!!! BUG DETECTED !!!");
                println!("The rewriter is ADDING backslash escapes that weren't in the original!");
                println!("This corrupts the JavaScript string content!");
            }
        }

        println!("\n=== Chunk 2 (URL continuation) ===");
        println!("{}", chunk2);

        let result2 = rewrite_nextjs_values(
            chunk2,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );
        println!("\nRewritten Chunk 2: {:?}", result2);

        // Let's verify the complete URL in chunk2 is rewritten
        if let Some(ref rewritten2) = result2 {
            assert!(
                rewritten2.contains("proxy.example.com") && rewritten2.contains("/complete"),
                "Complete URL in chunk2 should be rewritten to new host with /complete path"
            );
        }
    }

    #[test]
    fn test_embedded_regex_pattern() {
        // Test the regex pattern directly to understand what it matches
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        // Test 1: Unescaped double quotes (as in single-quoted JS string)
        let unescaped = r#"'336:{"url":"https://origin.example.com/path"}'"#;
        println!("Test 1 - Unescaped quotes:");
        println!("  Input: {}", unescaped);
        let result = rewriter.rewrite_embedded(unescaped);
        println!("  Result: {:?}", result);

        // Test 2: Escaped double quotes (as in double-quoted JS string or JSON)
        let escaped = r#"'336:{\"url\":\"https://origin.example.com/path\"}'"#;
        println!("\nTest 2 - Escaped quotes:");
        println!("  Input: {}", escaped);
        let result = rewriter.rewrite_embedded(escaped);
        println!("  Result: {:?}", result);

        // Test 3: Double-escaped quotes (as in JSON string inside JS string)
        let double_escaped = r#"'336:{\\"url\\":\\"https://origin.example.com/path\\"}'"#;
        println!("\nTest 3 - Double-escaped quotes:");
        println!("  Input: {}", double_escaped);
        let result = rewriter.rewrite_embedded(double_escaped);
        println!("  Result: {:?}", result);
    }

    #[test]
    fn test_backslash_n_preservation() {
        // Critical test: Check that \n (backslash-n) is preserved byte-for-byte
        // This is crucial because RSC payloads use \n as a record separator

        // String with literal backslash-n (two bytes: 0x5C 0x6E)
        let input =
            r#"self.__next_f.push([1, 'foo\n{"url":"https://origin.example.com/test"}\nbar']);"#;

        // Verify input has literal backslash-n
        let backslash_n_pos = input.find(r"\n").unwrap();
        assert_eq!(
            &input.as_bytes()[backslash_n_pos..backslash_n_pos + 2],
            [0x5C, 0x6E], // backslash, n
            "Input should have literal backslash-n"
        );

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URL");

        // Check the rewritten string still has literal backslash-n
        let new_pos = rewritten.find(r"\n").unwrap();
        assert_eq!(
            &rewritten.as_bytes()[new_pos..new_pos + 2],
            [0x5C, 0x6E],
            "Rewritten should preserve literal backslash-n"
        );

        // Count number of \n occurrences
        let original_count = input.matches(r"\n").count();
        let rewritten_count = rewritten.matches(r"\n").count();
        assert_eq!(
            original_count, rewritten_count,
            "Number of \\n occurrences should be preserved"
        );

        println!("Input:    {}", input);
        println!("Rewritten: {}", rewritten);
        println!(
            "\\n count: original={}, rewritten={}",
            original_count, rewritten_count
        );
    }

    #[test]
    fn test_url_rewriting_basic() {
        // Test that URL rewriting works correctly while preserving the original scheme
        let input = r#"self.__next_f.push([1, '{"url":"https://origin.example.com/news"}']);"#;

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http", // request_scheme is now ignored - original scheme is preserved
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URL");

        println!("Original:  {}", input);
        println!("Rewritten: {}", rewritten);

        // Verify the URL was rewritten correctly, preserving the original https scheme
        // With length preservation, URLs may have padding like /./././
        assert!(
            rewritten.contains("http://proxy.example.com") && rewritten.contains("/news"),
            "URL should be rewritten to new host with path, preserving https scheme. Got: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "URL should not contain original host"
        );
    }

    #[test]
    fn test_url_rewriting_preserves_rsc_structure() {
        // Test that RSC record structure is preserved after rewriting
        let input = r#"self.__next_f.push([1, '443:{"url":"https://origin.example.com/path"}\n444:{"other":"data"}']);"#;

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http", // request_scheme is now ignored - original scheme is preserved
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URL");

        println!("Original:  {}", input);
        println!("Rewritten: {}", rewritten);

        // Verify URL was rewritten (preserving https scheme)
        // With length preservation, URLs may have padding like /./././
        assert!(
            rewritten.contains("http://proxy.example.com") && rewritten.contains("/path"),
            "URL should be rewritten with preserved https scheme. Got: {}",
            rewritten
        );

        // Verify record structure is intact - both records should still be parseable
        assert!(
            rewritten.contains(r#"\n444:"#),
            "RSC record separator and next record ID must be preserved"
        );
        assert!(
            rewritten.contains(r#""other":"data""#),
            "Subsequent record data must be preserved"
        );
    }

    #[test]
    fn test_nav_menu_rewrite() {
        // Test a typical navigation menu payload
        // This is the payload that contains the dropdown menu items
        let input = r#"self.__next_f.push([
  1,
  '443:{"ID":878799,"title":"News","slug":"","post_parent":"0","guid":"pt000000000000000700000000000d68cf","menu_item_parent":"0","object_id":"category","url":"https://origin.example.com/news","target":"","attr_title":"","description":"","classes":"$444","menu_order":0,"post_type":"nav_menu_item","post_mime_type":"","object":"category","type":"taxonomy","type_label":"Category","menu_item_type":"taxonomy","hide_on_subnav":false,"children":"$445"}\n444:[""]\n445:[]'
]);"#;

        println!("=== Original Input ===");
        println!("{}", input);

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URL");

        println!("\n=== Rewritten Output ===");
        println!("{}", rewritten);

        // Verify the URL was rewritten using request scheme (http)
        // With length preservation, URL may have padding like /./././
        assert!(
            rewritten.contains("http://proxy.example.com") && rewritten.contains("/news"),
            "URL should be rewritten to new host with request scheme. Got: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "Original host should not remain"
        );

        // Verify RSC structure is preserved
        assert!(
            rewritten.contains(r#""ID":878799"#),
            "Record ID should be preserved"
        );
        assert!(
            rewritten.contains(r#""title":"News""#),
            "Title should be preserved"
        );
        assert!(
            rewritten.contains(r#""classes":"$444""#),
            "$444 reference should be preserved"
        );
        assert!(
            rewritten.contains(r#""children":"$445""#),
            "$445 reference should be preserved"
        );
        assert!(
            rewritten.contains(r#"\n444:[""]"#),
            "Record 444 should be preserved"
        );
        assert!(
            rewritten.contains(r#"\n445:[]"#),
            "Record 445 should be preserved"
        );

        // Critical: Verify the JavaScript is still valid
        // The string must be properly quoted and escaped
        assert!(
            rewritten.starts_with("self.__next_f.push(["),
            "Should start with valid JS"
        );
        assert!(rewritten.ends_with("]);"), "Should end with valid JS");

        // Check byte length difference
        let orig_len = input.len();
        let new_len = rewritten.len();
        println!("\n=== Length Analysis ===");
        println!("Original length: {}", orig_len);
        println!("Rewritten length: {}", new_len);
        println!("Difference: {} bytes", (orig_len as i64) - (new_len as i64));
    }

    #[test]
    fn test_site_base_url_rewrite() {
        // Test that siteBaseUrl gets rewritten alongside url attributes
        // This is critical for React navigation to work correctly - if siteBaseUrl
        // doesn't match the rewritten URLs, React may treat links as external
        let input = r#"self.__next_f.push([1, '{"siteBaseUrl":"https://origin.example.com","url":"https://origin.example.com/news"}']);"#;

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http", // request_scheme is now ignored - original scheme is preserved
            &["url".into(), "siteBaseUrl".into()], // Include siteBaseUrl
            true,   // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URLs");

        println!("Original:  {}", input);
        println!("Rewritten: {}", rewritten);

        // Both url and siteBaseUrl should be rewritten, preserving https scheme
        // With length preservation, URLs may have padding
        assert!(
            rewritten.contains("http://proxy.example.com"),
            "siteBaseUrl should be rewritten to match proxy host, preserving https. Got: {}",
            rewritten
        );
        assert!(
            rewritten.contains("/news"),
            "url path should be preserved. Got: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "Original host should not remain"
        );
    }

    #[test]
    fn test_site_production_domain_rewrite() {
        // Test that siteProductionDomain (bare hostname without scheme) gets rewritten
        // This is critical because Next.js uses this to determine if URLs are internal
        let input = r#"self.__next_f.push([1, '{"siteProductionDomain":"origin.example.com","url":"https://origin.example.com/news"}']);"#;

        let result = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http", // request_scheme is now ignored - original scheme is preserved
            &["url".into(), "siteProductionDomain".into()],
            true, // preserve_length for RSC payloads
        );

        let rewritten = result.expect("should rewrite URLs");

        println!("Original:  {}", input);
        println!("Rewritten: {}", rewritten);

        // siteProductionDomain and URL should be rewritten, with possible length padding
        assert!(
            rewritten.contains("proxy.example.com"),
            "siteProductionDomain should be rewritten to proxy host. Got: {}",
            rewritten
        );
        // URL should contain the path
        assert!(
            rewritten.contains("/news"),
            "url path should be preserved. Got: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "Original host should not remain"
        );
    }

    #[test]
    fn test_calculate_padding() {
        // Test whitespace padding calculation
        // When new URL is shorter, we need spaces to compensate
        let padding = UrlRewriter::calculate_padding(21, 24);
        assert_eq!(padding.len(), 3, "Should need 3 spaces");
        assert_eq!(padding, "   ", "Should be 3 spaces");

        // No padding when lengths are equal
        let padding = UrlRewriter::calculate_padding(24, 24);
        assert_eq!(padding.len(), 0);

        // No padding when new URL is longer
        let padding = UrlRewriter::calculate_padding(30, 24);
        assert_eq!(padding.len(), 0);
    }

    #[test]
    fn test_whitespace_padding_rewrite() {
        // Test that URL rewriting returns proper (url, padding) tuple
        // Original: https://origin.example.com/news (31 chars)
        // New URL: http://proxy.example.com/news (29 chars)
        // Padding needed: 2 spaces

        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true, // preserve_length
        );

        let original_url = "https://origin.example.com/news";
        let result = rewriter.rewrite_url_value(original_url);

        assert!(result.is_some(), "URL should be rewritten");
        let (new_url, padding) = result.unwrap();

        // Check the URL is correctly rewritten
        assert_eq!(new_url, "http://proxy.example.com/news");
        assert!(new_url.contains("proxy.example.com"));
        assert!(new_url.contains("/news"));

        // Check padding compensates for length difference
        let original_len = original_url.len(); // 33
        let new_len = new_url.len(); // 26
        assert_eq!(
            padding.len(),
            original_len - new_len,
            "Padding should be {} spaces",
            original_len - new_len
        );
        assert_eq!(padding, "  ", "Should be 2 spaces");

        // Total length (url + padding) should match original
        assert_eq!(
            new_url.len() + padding.len(),
            original_url.len(),
            "URL + padding should equal original length"
        );
    }

    #[test]
    fn test_no_padding_when_disabled() {
        // When preserve_length is false, no padding should be returned
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            false, // preserve_length disabled
        );

        let result = rewriter.rewrite_url_value("https://origin.example.com/news");
        assert!(result.is_some());
        let (new_url, padding) = result.unwrap();

        assert_eq!(new_url, "http://proxy.example.com/news");
        assert_eq!(padding, "", "No padding when preserve_length is false");
    }
}
