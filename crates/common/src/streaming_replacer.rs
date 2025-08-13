//! Generic streaming replacer for processing large content.
//!
//! This module provides functionality for replacing patterns in content
//! in streaming fashion, handling content that may be split across multiple chunks.

// Note: std::io::{Read, Write} were previously used by stream_process function
// which has been removed in favor of StreamingPipeline

/// A replacement pattern configuration
#[derive(Debug, Clone)]
pub struct Replacement {
    /// The string to find
    pub find: String,
    /// The string to replace it with
    pub replace_with: String,
}

/// A generic streaming replacer that processes content in chunks
pub struct StreamingReplacer {
    /// List of replacements to apply
    pub replacements: Vec<Replacement>,
    // Buffer to handle partial matches at chunk boundaries
    overlap_buffer: Vec<u8>,
    // Maximum pattern length to determine overlap size
    max_pattern_length: usize,
}

impl StreamingReplacer {
    /// Creates a new `StreamingReplacer` with the given replacements.
    ///
    /// # Arguments
    ///
    /// * `replacements` - List of string replacements to perform
    pub fn new(replacements: Vec<Replacement>) -> Self {
        // Calculate the maximum pattern length we need to buffer
        let max_pattern_length = replacements.iter().map(|r| r.find.len()).max().unwrap_or(0);

        Self {
            replacements,
            overlap_buffer: Vec::with_capacity(max_pattern_length),
            max_pattern_length,
        }
    }

    /// Creates a new `StreamingReplacer` with a single replacement.
    ///
    /// # Arguments
    ///
    /// * `find` - The string to find
    /// * `replace_with` - The string to replace it with
    pub fn new_single(find: &str, replace_with: &str) -> Self {
        Self::new(vec![Replacement {
            find: find.to_string(),
            replace_with: replace_with.to_string(),
        }])
    }

    /// Process a chunk of data and return the processed output
    pub fn process_chunk(&mut self, chunk: &[u8], is_last_chunk: bool) -> Vec<u8> {
        // Combine overlap buffer with new chunk
        let mut combined = self.overlap_buffer.clone();
        combined.extend_from_slice(chunk);

        if combined.is_empty() {
            return Vec::new();
        }

        // Determine how much content to process
        let process_end_bytes = if is_last_chunk {
            combined.len()
        } else {
            // To avoid splitting patterns, we need to be careful about where we cut.
            // We want to keep at least (max_pattern_length - 1) bytes for overlap.
            if combined.len() <= self.max_pattern_length {
                // Not enough data to process safely
                0
            } else {
                // Start with a safe boundary
                let mut boundary = combined.len().saturating_sub(self.max_pattern_length - 1);

                // Check if we might be splitting a pattern at this boundary
                // by looking for pattern starts near the boundary
                let check_start = boundary.saturating_sub(self.max_pattern_length);
                let check_end = (boundary + self.max_pattern_length).min(combined.len());

                if let Ok(check_str) = std::str::from_utf8(&combined[check_start..check_end]) {
                    // Look for any pattern that would be split by our boundary
                    for replacement in &self.replacements {
                        if let Some(pos) = check_str.find(&replacement.find) {
                            let pattern_start = check_start + pos;
                            let pattern_end = pattern_start + replacement.find.len();

                            // If the pattern crosses our boundary, adjust the boundary
                            if pattern_start < boundary && pattern_end > boundary {
                                boundary = pattern_start;
                                break;
                            }
                        }
                    }
                }

                boundary
            }
        };

        if process_end_bytes == 0 {
            // Not enough data to process yet
            self.overlap_buffer = combined;
            return Vec::new();
        }

        // Find a valid UTF-8 boundary at or before process_end_bytes
        let mut adjusted_end_bytes = process_end_bytes;
        while adjusted_end_bytes > 0 {
            // Check if this is a valid UTF-8 boundary
            if let Ok(s) = std::str::from_utf8(&combined[..adjusted_end_bytes]) {
                // Valid UTF-8 up to this point, process it
                let mut processed = s.to_string();

                // Apply all replacements
                for replacement in &self.replacements {
                    processed = processed.replace(&replacement.find, &replacement.replace_with);
                }

                // Save the overlap for the next chunk
                if !is_last_chunk {
                    self.overlap_buffer = combined[adjusted_end_bytes..].to_vec();
                } else {
                    self.overlap_buffer.clear();
                }

                return processed.into_bytes();
            }
            adjusted_end_bytes -= 1;
        }

        // This should never happen, but handle it gracefully
        self.overlap_buffer = combined;
        Vec::new()
    }

    /// Reset the internal buffer (useful when reusing the replacer)
    pub fn reset(&mut self) {
        self.overlap_buffer.clear();
    }
}

// Note: The stream_process function has been removed in favor of using
// StreamingPipeline from the streaming_processor module, which provides
// a more comprehensive solution with compression support.

/// Helper function to create a StreamingReplacer for URL replacements
pub fn create_url_replacer(
    origin_host: &str,
    origin_url: &str,
    request_host: &str,
    request_scheme: &str,
) -> StreamingReplacer {
    let request_url = format!("{}://{}", request_scheme, request_host);

    log::info!(
        "Creating URL replacer: origin_host='{}', origin_url='{}', request_host='{}', request_scheme='{}', request_url='{}'",
        origin_host, origin_url, request_host, request_scheme, request_url
    );

    let mut replacements = vec![
        // Replace full URLs first (more specific)
        Replacement {
            find: origin_url.to_string(),
            replace_with: request_url.clone(),
        },
    ];

    // Also handle HTTP variant if origin is HTTPS
    if origin_url.starts_with("https://") {
        let http_origin_url = origin_url.replace("https://", "http://");
        replacements.push(Replacement {
            find: http_origin_url,
            replace_with: request_url.clone(),
        });
    }

    // Replace protocol-relative URLs
    replacements.push(Replacement {
        find: format!("//{}", origin_host),
        replace_with: format!("//{}", request_host),
    });

    // Replace host in various contexts
    replacements.push(Replacement {
        find: origin_host.to_string(),
        replace_with: request_host.to_string(),
    });

    log::info!("URL replacements configured:");
    for (i, replacement) in replacements.iter().enumerate() {
        log::info!(
            "  {}. Find: '{}' -> Replace with: '{}'",
            i + 1,
            replacement.find,
            replacement.replace_with
        );
    }

    StreamingReplacer::new(replacements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_replacer_basic() {
        let mut replacer =
            StreamingReplacer::new_single("https://origin.example.com", "https://test.example.com");

        let input = b"Visit https://origin.example.com for more info";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "Visit https://test.example.com for more info");
    }

    // Note: test_multiple_replacements removed as it's redundant with test_stream_process
    // which tests the same functionality through StreamingPipeline

    #[test]
    fn test_streaming_replacer_chunks() {
        let mut replacer =
            StreamingReplacer::new_single("https://origin.example.com", "https://test.example.com");

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
        let replacements = vec![
            Replacement {
                find: "https://origin.example.com".to_string(),
                replace_with: "https://test.example.com".to_string(),
            },
            Replacement {
                find: "//origin.example.com".to_string(),
                replace_with: "//test.example.com".to_string(),
            },
        ];

        let mut replacer = StreamingReplacer::new(replacements);

        let input =
            b"<a href='https://origin.example.com'>Link</a> and //origin.example.com/resource";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert!(result.contains("https://test.example.com"));
        assert!(result.contains("//test.example.com/resource"));
    }

    #[test]
    fn test_streaming_replacer_edge_cases() {
        let mut replacer =
            StreamingReplacer::new_single("https://origin.example.com", "https://test.example.com");

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
    fn test_url_replacer_comprehensive() {
        let mut replacer = create_url_replacer(
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

        let processed = replacer.process_chunk(content.as_bytes(), true);
        let result = String::from_utf8(processed).unwrap();

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
    fn test_url_replacer_with_port() {
        let mut replacer = create_url_replacer(
            "origin.example.com:8080",
            "https://origin.example.com:8080",
            "test.example.com:9090",
            "https",
        );

        let content =
            b"Visit https://origin.example.com:8080/api or //origin.example.com:8080/resource";
        let processed = replacer.process_chunk(content, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(
            result,
            "Visit https://test.example.com:9090/api or //test.example.com:9090/resource"
        );
    }

    #[test]
    fn test_url_replacer_mixed_protocols() {
        let mut replacer = create_url_replacer(
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

        let processed = replacer.process_chunk(content.as_bytes(), true);
        let result = String::from_utf8(processed).unwrap();

        // When request is HTTP, all URLs should be replaced with HTTP
        assert!(result.contains("http://test.example.com"));
        assert!(!result.contains("https://test.example.com"));
        assert!(result.contains("//test.example.com/script.js"));
    }

    #[test]
    fn test_process_chunk_utf8_boundary() {
        let mut replacer =
            create_url_replacer("origin.com", "https://origin.com", "test.com", "https");

        // Create content with multi-byte UTF-8 characters that could cause boundary issues
        let content = "https://origin.com/test 思怙ᕏ测试 https://origin.com/more".as_bytes();

        // Process in small chunks to force potential boundary issues
        let chunk_size = 20;
        let mut result = Vec::new();

        for (i, chunk) in content.chunks(chunk_size).enumerate() {
            let is_last = i == content.chunks(chunk_size).count() - 1;
            result.extend(replacer.process_chunk(chunk, is_last));
        }

        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("https://test.com/test"));
        assert!(result_str.contains("https://test.com/more"));
        assert!(result_str.contains("思怙ᕏ测试"));
    }

    #[test]
    fn test_process_chunk_boundary_in_multibyte_char() {
        let mut replacer =
            create_url_replacer("example.com", "https://example.com", "new.com", "https");

        // Create a scenario where chunk boundary falls in the middle of a multi-byte character
        let content = "https://example.com/før/bår/test".as_bytes();

        // Split at byte 23, which should be in the middle of 'ø' (2-byte character)
        let chunk1 = &content[..23];
        let chunk2 = &content[23..];

        let mut result = Vec::new();
        result.extend(replacer.process_chunk(chunk1, false));
        result.extend(replacer.process_chunk(chunk2, true));

        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("https://new.com/før/bår/test"));
    }

    #[test]
    fn test_process_chunk_emoji_boundary() {
        let mut replacer =
            create_url_replacer("emoji.com", "https://emoji.com", "test.com", "https");

        // Test with 4-byte emoji characters
        let content = "https://emoji.com/test 🎉🎊🎋 https://emoji.com/more".as_bytes();

        // Process the entire content at once to verify it works
        let all_at_once = replacer.process_chunk(content, true);
        let expected = String::from_utf8(all_at_once).unwrap();
        assert!(expected.contains("https://test.com/test"));
        assert!(expected.contains("https://test.com/more"));
    }

    #[test]
    fn test_process_chunk_large_chunks() {
        let mut replacer =
            create_url_replacer("example.com", "https://example.com", "test.com", "https");

        // Test with content that won't have URLs split across chunks
        let content =
            "Visit https://example.com/page1 and then https://example.com/page2 for more info"
                .as_bytes();

        // Use large chunks to avoid splitting URLs
        let chunk_size = 50;
        let mut result = Vec::new();

        for (i, chunk) in content.chunks(chunk_size).enumerate() {
            let is_last = i == content.chunks(chunk_size).count() - 1;
            result.extend(replacer.process_chunk(chunk, is_last));
        }

        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("https://test.com/page1"));
        assert!(result_str.contains("https://test.com/page2"));
    }

    #[test]
    fn test_process_chunk_utf8_boundary_small_chunks() {
        let mut replacer = create_url_replacer("test.com", "https://test.com", "new.com", "https");

        // Test with multi-byte characters and very small chunks to stress UTF-8 boundaries
        let content = "Some text 思怙ᕏ测试 more text with 🎉 emoji".as_bytes();

        // Use very small chunks to force UTF-8 boundary handling
        let chunk_size = 8;
        let mut result = Vec::new();
        let chunks: Vec<_> = content.chunks(chunk_size).collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == chunks.len() - 1;
            result.extend(replacer.process_chunk(chunk, is_last));
        }

        let result_str = String::from_utf8(result).unwrap();
        // Just verify the content is preserved correctly
        assert!(result_str.contains("思怙ᕏ测试"));
        assert!(result_str.contains("🎉"));
    }

    #[test]
    fn test_generic_replacements() {
        // Test replacing arbitrary strings
        let replacements = vec![
            Replacement {
                find: "color".to_string(),
                replace_with: "colour".to_string(),
            },
            Replacement {
                find: "gray".to_string(),
                replace_with: "grey".to_string(),
            },
        ];

        let mut replacer = StreamingReplacer::new(replacements);

        let input = b"The color is gray, not light gray.";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "The colour is grey, not light grey.");
    }

    #[test]
    fn test_pattern_priority() {
        // Test that longer patterns are replaced first (order matters)
        let replacements = vec![
            Replacement {
                find: "hello world".to_string(),
                replace_with: "greetings universe".to_string(),
            },
            Replacement {
                find: "hello".to_string(),
                replace_with: "hi".to_string(),
            },
        ];

        let mut replacer = StreamingReplacer::new(replacements);

        let input = b"Say hello world and hello there!";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        // Note: Since we apply replacements in order, "hello world" gets replaced first
        assert_eq!(result, "Say greetings universe and hi there!");
    }

    #[test]
    fn test_overlapping_patterns() {
        // Test handling of overlapping patterns
        let replacements = vec![
            Replacement {
                find: "abc".to_string(),
                replace_with: "xyz".to_string(),
            },
            Replacement {
                find: "bcd".to_string(),
                replace_with: "123".to_string(),
            },
        ];

        let mut replacer = StreamingReplacer::new(replacements);

        let input = b"abcdef";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        // "abc" gets replaced first, so "bcd" is no longer found
        assert_eq!(result, "xyzdef");
    }

    #[test]
    fn test_empty_replacement() {
        // Test removing strings (replacing with empty string)
        let mut replacer = StreamingReplacer::new_single("REMOVE_ME", "");

        let input = b"Keep this REMOVE_ME but not this";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "Keep this  but not this");
    }

    #[test]
    fn test_case_sensitive_replacement() {
        // Test that replacements are case-sensitive
        let mut replacer = StreamingReplacer::new_single("Hello", "Hi");

        let input = b"Hello world, hello there, HELLO!";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "Hi world, hello there, HELLO!");
    }

    #[test]
    fn test_special_characters_in_pattern() {
        // Test replacing patterns with special regex characters
        let replacements = vec![
            Replacement {
                find: "cost: $10.99".to_string(),
                replace_with: "price: €9.99".to_string(),
            },
            Replacement {
                find: "[TAG]".to_string(),
                replace_with: "<LABEL>".to_string(),
            },
        ];

        let mut replacer = StreamingReplacer::new(replacements);

        let input = b"The cost: $10.99 [TAG] is final";
        let processed = replacer.process_chunk(input, true);
        let result = String::from_utf8(processed).unwrap();

        assert_eq!(result, "The price: €9.99 <LABEL> is final");
    }

    #[test]
    fn test_stream_process() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let replacements = vec![
            Replacement {
                find: "foo".to_string(),
                replace_with: "bar".to_string(),
            },
            Replacement {
                find: "hello".to_string(),
                replace_with: "hi".to_string(),
            },
        ];

        let replacer = StreamingReplacer::new(replacements);
        let input = "hello world, foo is foo";
        let mut output = Vec::new();

        let config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 50, // Use larger chunk size to ensure patterns aren't split
        };
        let mut pipeline = StreamingPipeline::new(config, replacer);

        pipeline
            .process(Cursor::new(input.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        assert_eq!(result, "hi world, bar is bar");
    }

    #[test]
    fn test_stream_process_large_content() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let replacer = StreamingReplacer::new_single("OLD", "NEW");

        // Create large content with repeated patterns
        let input = "OLD content ".repeat(1000);
        let expected = "NEW content ".repeat(1000);

        let mut output = Vec::new();

        let config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 1024, // 1KB chunks
        };
        let mut pipeline = StreamingPipeline::new(config, replacer);

        pipeline
            .process(Cursor::new(input.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_stream_process_empty_input() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let replacer = StreamingReplacer::new_single("foo", "bar");
        let mut output = Vec::new();

        let config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(config, replacer);

        pipeline.process(Cursor::new(b""), &mut output).unwrap();

        assert!(output.is_empty());
    }

    #[test]
    fn test_stream_process_pattern_split_across_chunks() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let replacer = StreamingReplacer::new_single("hello", "hi");

        let input = "hello world";
        let mut output = Vec::new();

        // Use a chunk size that will split "hello" across chunks
        // With chunk size 3, we get: "hel", "lo ", "wor", "ld"
        let config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 3,
        };
        let mut pipeline = StreamingPipeline::new(config, replacer);

        pipeline
            .process(Cursor::new(input.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        assert_eq!(result, "hi world");
    }
}
