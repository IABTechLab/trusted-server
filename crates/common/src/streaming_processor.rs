//! Unified streaming processor architecture for handling compressed and uncompressed content.
//!
//! This module provides a flexible pipeline for processing content streams with:
//! - Automatic compression/decompression handling
//! - Pluggable content processors (text replacement, HTML rewriting, etc.)
//! - Memory-efficient streaming
//! - UTF-8 boundary handling

use error_stack::{Report, ResultExt};
use std::io::{self, Read, Write};

use crate::error::TrustedServerError;

/// Trait for streaming content processors
pub trait StreamProcessor {
    /// Process a chunk of data
    ///
    /// # Arguments
    /// * `chunk` - The data chunk to process
    /// * `is_last` - Whether this is the last chunk
    ///
    /// # Returns
    /// Processed data or error
    ///
    /// # Errors
    ///
    /// Returns an error if processing fails (e.g., I/O errors, encoding issues).
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error>;

    /// Reset the processor state (useful for reuse)
    fn reset(&mut self) {}
}

/// Compression type for the stream
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Compression {
    None,
    Gzip,
    Deflate,
    Brotli,
}

impl Compression {
    /// Detect compression from content-encoding header
    #[must_use]
    pub fn from_content_encoding(encoding: &str) -> Self {
        match encoding.to_lowercase().as_str() {
            "gzip" => Self::Gzip,
            "deflate" => Self::Deflate,
            "br" => Self::Brotli,
            _ => Self::None,
        }
    }
}

/// Configuration for the streaming pipeline
pub struct PipelineConfig {
    /// Input compression type
    pub input_compression: Compression,
    /// Output compression type (usually same as input)
    pub output_compression: Compression,
    /// Chunk size for reading
    pub chunk_size: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192, // 8KB default
        }
    }
}

/// Main streaming pipeline that handles compression and processing
pub struct StreamingPipeline<P: StreamProcessor> {
    config: PipelineConfig,
    processor: P,
}

impl<P: StreamProcessor> StreamingPipeline<P> {
    /// Create a new streaming pipeline
    ///
    /// # Errors
    ///
    /// No errors are returned by this constructor.
    pub fn new(config: PipelineConfig, processor: P) -> Self {
        Self { config, processor }
    }

    /// Process a stream from input to output
    ///
    /// # Errors
    ///
    /// Returns an error if the compression transformation is unsupported or if reading/writing fails.
    pub fn process<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        match (
            self.config.input_compression,
            self.config.output_compression,
        ) {
            (Compression::None, Compression::None) => self.process_uncompressed(input, output),
            (Compression::Gzip, Compression::Gzip) => self.process_gzip_to_gzip(input, output),
            (Compression::Gzip, Compression::None) => self.process_gzip_to_none(input, output),
            (Compression::Deflate, Compression::Deflate) => {
                self.process_deflate_to_deflate(input, output)
            }
            (Compression::Deflate, Compression::None) => {
                self.process_deflate_to_none(input, output)
            }
            (Compression::Brotli, Compression::Brotli) => {
                self.process_brotli_to_brotli(input, output)
            }
            (Compression::Brotli, Compression::None) => self.process_brotli_to_none(input, output),
            _ => Err(Report::new(TrustedServerError::Proxy {
                message: "Unsupported compression transformation".to_string(),
            })),
        }
    }

    /// Process uncompressed stream
    fn process_uncompressed<R: Read, W: Write>(
        &mut self,
        mut input: R,
        mut output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut buffer = vec![0u8; self.config.chunk_size];

        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    // End of stream - process any remaining data
                    let final_chunk = self.processor.process_chunk(&[], true).change_context(
                        TrustedServerError::Proxy {
                            message: "Failed to process final chunk".to_string(),
                        },
                    )?;
                    if !final_chunk.is_empty() {
                        output.write_all(&final_chunk).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write final chunk".to_string(),
                            },
                        )?;
                    }
                    break;
                }
                Ok(n) => {
                    // Process this chunk
                    let processed = self
                        .processor
                        .process_chunk(&buffer[..n], false)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to process chunk".to_string(),
                        })?;
                    if !processed.is_empty() {
                        output
                            .write_all(&processed)
                            .change_context(TrustedServerError::Proxy {
                                message: "Failed to write processed chunk".to_string(),
                            })?;
                    }
                }
                Err(e) => {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: format!("Failed to read from input: {}", e),
                    }));
                }
            }
        }

        output.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush output".to_string(),
        })?;

        Ok(())
    }

    /// Process gzip compressed stream
    fn process_gzip_to_gzip<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Decompress input
        let mut decoder = GzDecoder::new(input);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to decompress gzip".to_string(),
            })?;

        log::info!("Decompressed size: {} bytes", decompressed.len());

        // Process the decompressed content
        let processed = self
            .processor
            .process_chunk(&decompressed, true)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process content".to_string(),
            })?;

        log::info!("Processed size: {} bytes", processed.len());

        // Recompress the output
        let mut encoder = GzEncoder::new(output, Compression::default());
        encoder
            .write_all(&processed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write to gzip encoder".to_string(),
            })?;
        encoder.finish().change_context(TrustedServerError::Proxy {
            message: "Failed to finish gzip encoder".to_string(),
        })?;

        Ok(())
    }

    /// Process gzip compressed input to uncompressed output (decompression only)
    fn process_gzip_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        mut output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::GzDecoder;

        // Decompress input
        let mut decoder = GzDecoder::new(input);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to decompress gzip".to_string(),
            })?;

        log::info!("Decompressed size: {} bytes", decompressed.len());

        // Process the decompressed content
        let processed = self
            .processor
            .process_chunk(&decompressed, true)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process content".to_string(),
            })?;

        log::info!("Processed size: {} bytes", processed.len());

        // Write uncompressed output
        output
            .write_all(&processed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write output".to_string(),
            })?;

        Ok(())
    }

    /// Process deflate compressed stream
    fn process_deflate_to_deflate<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::ZlibDecoder;
        use flate2::write::ZlibEncoder;
        use flate2::Compression;

        let decoder = ZlibDecoder::new(input);
        let encoder = ZlibEncoder::new(output, Compression::default());

        self.process_through_compression(decoder, encoder)
    }

    /// Process deflate compressed input to uncompressed output (decompression only)
    fn process_deflate_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        mut output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::ZlibDecoder;

        // Decompress input
        let mut decoder = ZlibDecoder::new(input);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to decompress deflate".to_string(),
            })?;

        log::info!(
            "Deflate->None decompressed size: {} bytes",
            decompressed.len()
        );

        // Process the decompressed content
        let processed = self
            .processor
            .process_chunk(&decompressed, true)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process content".to_string(),
            })?;

        log::info!("Deflate->None processed size: {} bytes", processed.len());

        // Write uncompressed output
        output
            .write_all(&processed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write output".to_string(),
            })?;

        Ok(())
    }

    /// Process brotli compressed stream
    fn process_brotli_to_brotli<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use brotli::enc::writer::CompressorWriter;
        use brotli::enc::BrotliEncoderParams;
        use brotli::Decompressor;

        let decoder = Decompressor::new(input, 4096);
        let params = BrotliEncoderParams {
            quality: 4,
            lgwin: 22,
            ..Default::default()
        };
        let encoder = CompressorWriter::with_params(output, 4096, &params);

        self.process_through_compression(decoder, encoder)
    }

    /// Process brotli compressed input to uncompressed output (decompression only)
    fn process_brotli_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        mut output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use brotli::Decompressor;

        // Decompress input
        let mut decoder = Decompressor::new(input, 4096);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to decompress brotli".to_string(),
            })?;

        log::info!(
            "Brotli->None decompressed size: {} bytes",
            decompressed.len()
        );

        // Process the decompressed content
        let processed = self
            .processor
            .process_chunk(&decompressed, true)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process content".to_string(),
            })?;

        log::info!("Brotli->None processed size: {} bytes", processed.len());

        // Write uncompressed output
        output
            .write_all(&processed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write output".to_string(),
            })?;

        Ok(())
    }

    /// Generic processing through compression layers
    fn process_through_compression<R: Read, W: Write>(
        &mut self,
        mut decoder: R,
        mut encoder: W,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut buffer = vec![0u8; self.config.chunk_size];

        loop {
            match decoder.read(&mut buffer) {
                Ok(0) => {
                    // End of stream
                    let final_chunk = self.processor.process_chunk(&[], true).change_context(
                        TrustedServerError::Proxy {
                            message: "Failed to process final chunk".to_string(),
                        },
                    )?;
                    if !final_chunk.is_empty() {
                        encoder.write_all(&final_chunk).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write final chunk".to_string(),
                            },
                        )?;
                    }
                    break;
                }
                Ok(n) => {
                    let processed = self
                        .processor
                        .process_chunk(&buffer[..n], false)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to process chunk".to_string(),
                        })?;
                    if !processed.is_empty() {
                        encoder.write_all(&processed).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write processed chunk".to_string(),
                            },
                        )?;
                    }
                }
                Err(e) => {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: format!("Failed to read from decoder: {}", e),
                    }));
                }
            }
        }

        // Flush encoder (this also finishes compression)
        encoder.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush encoder".to_string(),
        })?;

        // For GzEncoder and similar, we need to finish() to properly close the stream
        // The flush above might not be enough
        drop(encoder);

        Ok(())
    }
}

/// Adapter to use `lol_html` `HtmlRewriter` as a `StreamProcessor`
/// Important: Due to `lol_html`'s ownership model, we must accumulate input
/// and process it all at once when the stream ends. This is a limitation
/// of the `lol_html` library's API design.
pub struct HtmlRewriterAdapter {
    settings: lol_html::Settings<'static, 'static>,
    accumulated_input: Vec<u8>,
}

impl HtmlRewriterAdapter {
    /// Create a new HTML rewriter adapter
    #[must_use]
    pub fn new(settings: lol_html::Settings<'static, 'static>) -> Self {
        Self {
            settings,
            accumulated_input: Vec::new(),
        }
    }
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        // Accumulate input chunks
        self.accumulated_input.extend_from_slice(chunk);

        if !chunk.is_empty() {
            log::debug!(
                "Buffering chunk: {} bytes, total buffered: {} bytes",
                chunk.len(),
                self.accumulated_input.len()
            );
        }

        // Only process when we have all the input
        if is_last {
            log::info!(
                "Processing complete document: {} bytes",
                self.accumulated_input.len()
            );

            // Process all accumulated input at once
            let mut output = Vec::new();

            // Create rewriter with output sink
            let mut rewriter = lol_html::HtmlRewriter::new(
                std::mem::take(&mut self.settings),
                |chunk: &[u8]| {
                    output.extend_from_slice(chunk);
                },
            );

            // Process the entire document
            rewriter.write(&self.accumulated_input).map_err(|e| {
                log::error!("Failed to process HTML: {}", e);
                io::Error::other(format!("HTML processing failed: {}", e))
            })?;

            // Finalize the rewriter
            rewriter.end().map_err(|e| {
                log::error!("Failed to finalize: {}", e);
                io::Error::other(format!("HTML finalization failed: {}", e))
            })?;

            log::debug!("Output size: {} bytes", output.len());
            self.accumulated_input.clear();
            Ok(output)
        } else {
            // Return empty until we have all input
            // This is a limitation of lol_html's API
            Ok(Vec::new())
        }
    }

    fn reset(&mut self) {
        self.accumulated_input.clear();
    }
}

/// Adapter to use our existing `StreamingReplacer` as a `StreamProcessor`
use crate::streaming_replacer::StreamingReplacer;

impl StreamProcessor for StreamingReplacer {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        Ok(self.process_chunk(chunk, is_last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming_replacer::{Replacement, StreamingReplacer};

    #[test]
    fn test_uncompressed_pipeline() {
        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_string(),
            replace_with: "hi".to_string(),
        }]);

        let config = PipelineConfig::default();
        let mut pipeline = StreamingPipeline::new(config, replacer);

        let input = b"hello world";
        let mut output = Vec::new();

        pipeline.process(&input[..], &mut output).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "hi world");
    }

    #[test]
    fn test_compression_detection() {
        assert_eq!(
            Compression::from_content_encoding("gzip"),
            Compression::Gzip
        );
        assert_eq!(
            Compression::from_content_encoding("GZIP"),
            Compression::Gzip
        );
        assert_eq!(
            Compression::from_content_encoding("deflate"),
            Compression::Deflate
        );
        assert_eq!(
            Compression::from_content_encoding("br"),
            Compression::Brotli
        );
        assert_eq!(
            Compression::from_content_encoding("identity"),
            Compression::None
        );
        assert_eq!(Compression::from_content_encoding(""), Compression::None);
    }

    #[test]
    fn test_html_rewriter_adapter_accumulates_until_last() {
        use lol_html::{element, Settings};

        // Create a simple HTML rewriter that replaces text
        let settings = Settings {
            element_content_handlers: vec![element!("p", |el| {
                el.set_inner_content("replaced", lol_html::html_content::ContentType::Text);
                Ok(())
            })],
            ..Settings::default()
        };

        let mut adapter = HtmlRewriterAdapter::new(settings);

        // Test that intermediate chunks return empty
        let chunk1 = b"<html><body>";
        let result1 = adapter.process_chunk(chunk1, false).unwrap();
        assert_eq!(result1.len(), 0, "Should return empty for non-last chunk");

        let chunk2 = b"<p>original</p>";
        let result2 = adapter.process_chunk(chunk2, false).unwrap();
        assert_eq!(result2.len(), 0, "Should return empty for non-last chunk");

        // Test that last chunk processes everything
        let chunk3 = b"</body></html>";
        let result3 = adapter.process_chunk(chunk3, true).unwrap();
        assert!(
            !result3.is_empty(),
            "Should return processed content for last chunk"
        );

        let output = String::from_utf8(result3).unwrap();
        assert!(output.contains("replaced"), "Should have replaced content");
        assert!(output.contains("<html>"), "Should have complete HTML");
    }

    #[test]
    fn test_html_rewriter_adapter_handles_large_input() {
        use lol_html::Settings;

        let settings = Settings::default();
        let mut adapter = HtmlRewriterAdapter::new(settings);

        // Create a large HTML document
        let mut large_html = String::from("<html><body>");
        for i in 0..1000 {
            large_html.push_str(&format!("<p>Paragraph {}</p>", i));
        }
        large_html.push_str("</body></html>");

        // Process in chunks
        let chunk_size = 1024;
        let bytes = large_html.as_bytes();
        let mut chunks = bytes.chunks(chunk_size);
        let mut last_chunk = chunks.next().unwrap_or(&[]);

        for chunk in chunks {
            let result = adapter.process_chunk(last_chunk, false).unwrap();
            assert_eq!(result.len(), 0, "Intermediate chunks should return empty");
            last_chunk = chunk;
        }

        // Process last chunk
        let result = adapter.process_chunk(last_chunk, true).unwrap();
        assert!(!result.is_empty(), "Last chunk should return content");

        let output = String::from_utf8(result).unwrap();
        assert!(
            output.contains("Paragraph 999"),
            "Should contain all content"
        );
    }

    #[test]
    fn test_html_rewriter_adapter_reset() {
        use lol_html::Settings;

        let settings = Settings::default();
        let mut adapter = HtmlRewriterAdapter::new(settings);

        // Process some content
        adapter.process_chunk(b"<html>", false).unwrap();
        adapter.process_chunk(b"<body>test</body>", false).unwrap();

        // Reset should clear accumulated input
        adapter.reset();

        // After reset, adapter should be ready for new input
        let result = adapter.process_chunk(b"<p>new</p>", true).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert_eq!(
            output, "<p>new</p>",
            "Should only contain new input after reset"
        );
    }

    #[test]
    fn test_streaming_pipeline_with_html_rewriter() {
        use lol_html::{element, Settings};

        let settings = Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href") {
                    if href.contains("example.com") {
                        el.set_attribute("href", &href.replace("example.com", "test.com"))?;
                    }
                }
                Ok(())
            })],
            ..Settings::default()
        };

        let adapter = HtmlRewriterAdapter::new(settings);
        let config = PipelineConfig::default();
        let mut pipeline = StreamingPipeline::new(config, adapter);

        let input = b"<html><body><a href=\"https://example.com\">Link</a></body></html>";
        let mut output = Vec::new();

        pipeline.process(&input[..], &mut output).unwrap();

        let result = String::from_utf8(output).unwrap();
        assert!(
            result.contains("https://test.com"),
            "Should have replaced URL"
        );
        assert!(
            !result.contains("example.com"),
            "Should not contain original URL"
        );
    }
}
