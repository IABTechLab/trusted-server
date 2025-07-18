//! Streaming URL replacer for processing large responses.
//!
//! This module provides functionality for replacing origin URLs with request URLs
//! in streaming fashion, handling content that may be split across multiple chunks.

/// A streaming replacer that processes content in chunks
pub struct StreamingReplacer {
    origin_host: String,
    origin_url: String,
    request_host: String,
    request_url: String,
    // Buffer to handle partial matches at chunk boundaries
    overlap_buffer: Vec<u8>,
    // Maximum pattern length to determine overlap size
    max_pattern_length: usize,
}

impl StreamingReplacer {
    /// Creates a new `StreamingReplacer` instance.
    ///
    /// # Arguments
    ///
    /// * `origin_host` - The origin hostname (e.g., "origin.example.com")
    /// * `origin_url` - The full origin URL (e.g., "https://origin.example.com")
    /// * `request_host` - The request hostname (e.g., "test.example.com")
    /// * `request_scheme` - The request scheme ("http" or "https")
    pub fn new(
        origin_host: &str,
        origin_url: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        let request_url = format!("{}://{}", request_scheme, request_host);

        // Calculate the maximum pattern length we need to buffer
        let patterns = vec![
            origin_url.len(),
            origin_host.len(),
            format!("//{}", origin_host).len(),
            // Account for HTTP variant if origin is HTTPS
            if origin_url.starts_with("https://") {
                origin_url.replace("https://", "http://").len()
            } else {
                0
            },
        ];

        let max_pattern_length = patterns.into_iter().max().unwrap_or(0);

        Self {
            origin_host: origin_host.to_string(),
            origin_url: origin_url.to_string(),
            request_host: request_host.to_string(),
            request_url,
            overlap_buffer: Vec::with_capacity(max_pattern_length),
            max_pattern_length,
        }
    }

    /// Process a chunk of data and return the processed output
    pub fn process_chunk(&mut self, chunk: &[u8], is_last_chunk: bool) -> Vec<u8> {
        // Combine overlap buffer with new chunk
        let mut combined = self.overlap_buffer.clone();
        combined.extend_from_slice(chunk);

        // Convert to string for processing (using lossy conversion)
        let content = String::from_utf8_lossy(&combined);

        // Determine how much content to process
        let process_end = if is_last_chunk {
            content.len()
        } else {
            // Keep the last max_pattern_length characters for the next chunk
            content.len().saturating_sub(self.max_pattern_length)
        };

        if process_end == 0 {
            // Not enough data to process yet
            self.overlap_buffer = combined;
            return Vec::new();
        }

        // Process the content up to process_end
        let to_process = &content[..process_end];

        // Use the replace_origin_urls method
        let processed = self.replace_origin_urls(
            to_process,
            self.request_url.split("://").nth(0).unwrap_or("https"),
        );

        // Save the overlap for the next chunk
        if !is_last_chunk {
            self.overlap_buffer = combined[process_end..].to_vec();
        } else {
            self.overlap_buffer.clear();
        }

        processed.into_bytes()
    }

    /// Replaces origin URLs in content with request URLs.
    ///
    /// This function performs the URL replacement logic.
    /// It replaces both the origin host and full origin URL with their request equivalents.
    ///
    /// # Arguments
    ///
    /// * `content` - The content to process
    /// * `request_scheme` - The request scheme ("http" or "https")
    ///
    /// # Returns
    ///
    /// The content with all origin references replaced
    pub fn replace_origin_urls(&self, content: &str, request_scheme: &str) -> String {
        let request_url = format!("{}://{}", request_scheme, self.request_host);

        log::info!("Replacing {} with {}", self.origin_url, request_url);

        // Start with the content
        let mut result = content.to_string();

        // Replace full URLs first (more specific)
        result = result.replace(&self.origin_url, &request_url);

        // Also try with http if origin was https (in case of mixed content)
        if self.origin_url.starts_with("https://") {
            let http_origin_url = self.origin_url.replace("https://", "http://");
            result = result.replace(&http_origin_url, &request_url);
        }

        // Replace protocol-relative URLs (//example.com)
        let protocol_relative_origin = format!("//{}", self.origin_host);
        let protocol_relative_request = format!("//{}", self.request_host);
        result = result.replace(&protocol_relative_origin, &protocol_relative_request);

        // Replace host in various contexts
        // This handles cases like: "host": "origin.example.com" in JSON
        result = result.replace(&self.origin_host, &self.request_host);

        // Log if replacements were made
        if result != content {
            log::debug!("URL replacements made in content");
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_replacer_basic() {
        let mut replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        let input = b"Visit https://origin.example.com for more info";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "Visit https://test.example.com for more info");
    }

    #[test]
    fn test_streaming_replacer_chunks() {
        let mut replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        // Test that patterns split across chunks are handled correctly
        let chunk1 = b"Visit https://origin.exam";
        let chunk2 = b"ple.com for more info";

        let processed1 = replacer.process_chunk(chunk1, false);
        let processed2 = replacer.process_chunk(chunk2, true);

        let result = String::from_utf8([processed1, processed2].concat()).unwrap();
        assert_eq!(result, "Visit https://test.example.com for more info");
    }

    #[test]
    fn test_streaming_replacer_multiple_patterns() {
        let mut replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        let input =
            b"<a href='https://origin.example.com'>Link</a> and //origin.example.com/resource";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert!(result.contains("https://test.example.com"));
        assert!(result.contains("//test.example.com/resource"));
    }

    #[test]
    fn test_streaming_replacer_edge_cases() {
        let mut replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        // Empty chunk
        let processed = replacer.process_chunk(b"", true);
        assert!(processed.is_empty());

        // Very small chunks
        let chunks = [
            b"h".as_ref(),
            b"t".as_ref(),
            b"t".as_ref(),
            b"p".as_ref(),
            b"s".as_ref(),
            b":".as_ref(),
            b"/".as_ref(),
            b"/".as_ref(),
            b"origin.example.com".as_ref(),
        ];

        let mut result = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == chunks.len() - 1;
            let processed = replacer.process_chunk(chunk, is_last);
            result.extend(processed);
        }

        let result_str = String::from_utf8(result).unwrap();
        assert_eq!(result_str, "https://test.example.com");
    }

    #[test]
    fn test_replace_origin_urls_comprehensive() {
        let replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        // Test comprehensive URL replacement scenarios
        let content = r#"
            <!-- Full HTTPS URLs -->
            <a href="https://origin.example.com/page">Link</a>
            
            <!-- HTTP URLs (should be upgraded to request scheme) -->
            <img src="http://origin.example.com/image.jpg">
            
            <!-- Protocol-relative URLs -->
            <script src="//origin.example.com/script.js"></script>
            
            <!-- JSON API responses -->
            {"api": "https://origin.example.com/api", "host": "origin.example.com"}
        "#;

        let result = replacer.replace_origin_urls(content, "https");

        // Verify all patterns were replaced
        assert!(result.contains("https://test.example.com/page"));
        assert!(result.contains("https://test.example.com/image.jpg"));
        assert!(result.contains("//test.example.com/script.js"));
        assert!(result.contains(r#""api": "https://test.example.com/api""#));
        assert!(result.contains(r#""host": "test.example.com""#));

        // Ensure no origin URLs remain
        assert!(!result.contains("origin.example.com"));
    }

    #[test]
    fn test_replace_origin_urls_with_port() {
        let replacer = StreamingReplacer::new(
            "origin.example.com:8080",
            "https://origin.example.com:8080",
            "test.example.com:9090",
            "https",
        );

        let content =
            "Visit https://origin.example.com:8080/api or //origin.example.com:8080/resource";
        let result = replacer.replace_origin_urls(content, "https");

        assert_eq!(
            result,
            "Visit https://test.example.com:9090/api or //test.example.com:9090/resource"
        );
    }

    #[test]
    fn test_replace_origin_urls_mixed_protocols() {
        let replacer = StreamingReplacer::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "http",
        );

        let content = r#"
            <a href="https://origin.example.com">HTTPS Link</a>
            <a href="http://origin.example.com">HTTP Link</a>
            <script src="//origin.example.com/script.js"></script>
        "#;

        let result = replacer.replace_origin_urls(content, "http");

        // When request is HTTP, all URLs should be replaced with HTTP
        assert!(result.contains("http://test.example.com"));
        assert!(!result.contains("https://test.example.com"));
        assert!(result.contains("//test.example.com/script.js"));
    }
}
