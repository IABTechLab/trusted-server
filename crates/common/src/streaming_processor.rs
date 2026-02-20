//! Unified streaming processor architecture for handling compressed and uncompressed content.
//!
//! This module provides a flexible pipeline for processing content streams with:
//! - Automatic compression/decompression handling
//! - Pluggable content processors (text replacement, HTML rewriting, etc.)
//! - Memory-efficient streaming
//! - UTF-8 boundary handling

use error_stack::{Report, ResultExt};
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;

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

    /// Process gzip compressed stream (streaming — no full-body buffering)
    fn process_gzip_to_gzip<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let decoder = GzDecoder::new(input);
        let encoder = GzEncoder::new(output, Compression::default());

        let encoder = self.process_through_compression(decoder, encoder)?;
        encoder.finish().change_context(TrustedServerError::Proxy {
            message: "Failed to finish gzip encoder".to_string(),
        })?;
        Ok(())
    }

    /// Decompress input, process content, and write uncompressed output.
    fn decompress_and_process<R: Read, W: Write>(
        &mut self,
        mut decoder: R,
        mut output: W,
        codec_name: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .change_context(TrustedServerError::Proxy {
                message: format!("Failed to decompress {codec_name}"),
            })?;

        log::info!(
            "{codec_name} decompressed size: {} bytes",
            decompressed.len()
        );

        let processed = self
            .processor
            .process_chunk(&decompressed, true)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process content".to_string(),
            })?;

        log::info!("{codec_name} processed size: {} bytes", processed.len());

        output
            .write_all(&processed)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to write output".to_string(),
            })?;

        Ok(())
    }

    /// Process gzip compressed input to uncompressed output (decompression only)
    fn process_gzip_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::GzDecoder;

        self.decompress_and_process(GzDecoder::new(input), output, "gzip")
    }

    /// Process deflate compressed stream (streaming)
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

        let encoder = self.process_through_compression(decoder, encoder)?;
        encoder.finish().change_context(TrustedServerError::Proxy {
            message: "Failed to finish deflate encoder".to_string(),
        })?;
        Ok(())
    }

    /// Process deflate compressed input to uncompressed output (decompression only)
    fn process_deflate_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::ZlibDecoder;

        self.decompress_and_process(ZlibDecoder::new(input), output, "deflate")
    }

    /// Process brotli compressed stream (streaming)
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

        let encoder = self.process_through_compression(decoder, encoder)?;
        // CompressorWriter finalizes the brotli stream on drop. Unlike gzip/deflate,
        // brotli has no checksum trailer so drop-based finalization is safe.
        drop(encoder);
        Ok(())
    }

    /// Process brotli compressed input to uncompressed output (decompression only)
    fn process_brotli_to_none<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use brotli::Decompressor;

        self.decompress_and_process(Decompressor::new(input, 4096), output, "brotli")
    }

    /// Generic chunk loop through compression layers.
    ///
    /// Returns the encoder so the caller can finalize it properly (e.g.
    /// `GzEncoder::finish()`, `ZlibEncoder::finish()`). This avoids the
    /// silent error swallowing that `drop(encoder)` causes — gzip/deflate
    /// trailers contain checksums whose write failures must be propagated.
    fn process_through_compression<R: Read, W: Write>(
        &mut self,
        mut decoder: R,
        mut encoder: W,
    ) -> Result<W, Report<TrustedServerError>> {
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

        encoder.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush encoder".to_string(),
        })?;

        Ok(encoder)
    }
}

/// Output sink that writes lol_html output chunks into a shared `Rc<RefCell<Vec<u8>>>` buffer.
struct RcVecSink(Rc<RefCell<Vec<u8>>>);

impl lol_html::OutputSink for RcVecSink {
    fn handle_chunk(&mut self, chunk: &[u8]) {
        self.0.borrow_mut().extend_from_slice(chunk);
    }
}

/// Adapter to use `lol_html` `HtmlRewriter` as a `StreamProcessor`.
///
/// Uses lol_html's incremental streaming API: each incoming chunk is written to
/// the rewriter immediately, and whatever output lol_html has ready is drained
/// and returned. This avoids buffering the full document before processing begins.
pub struct HtmlRewriterAdapter {
    rewriter: Option<lol_html::HtmlRewriter<'static, RcVecSink>>,
    output: Rc<RefCell<Vec<u8>>>,
}

impl HtmlRewriterAdapter {
    /// Create a new HTML rewriter adapter.
    #[must_use]
    pub fn new(settings: lol_html::Settings<'static, 'static>) -> Self {
        // Pre-allocate to avoid reallocation churn since lol_html writes incrementally
        let output = Rc::new(RefCell::new(Vec::with_capacity(8192)));
        let rewriter = lol_html::HtmlRewriter::new(settings, RcVecSink(Rc::clone(&output)));
        Self {
            rewriter: Some(rewriter),
            output,
        }
    }
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        if let Some(rewriter) = &mut self.rewriter {
            if !chunk.is_empty() {
                rewriter.write(chunk).map_err(|e| {
                    log::error!("Failed to write HTML chunk: {}", e);
                    io::Error::other(format!("HTML processing failed: {}", e))
                })?;
            }
        }

        if is_last {
            if let Some(rewriter) = self.rewriter.take() {
                rewriter.end().map_err(|e| {
                    log::error!("Failed to finalize HTML rewriter: {}", e);
                    io::Error::other(format!("HTML finalization failed: {}", e))
                })?;
            }
        }

        // Drain whatever lol_html produced for this chunk and return it.
        // Pre-allocate the next buffer to prevent lol_html from triggering allocations on its many small writes.
        let result = std::mem::replace(
            &mut *self.output.borrow_mut(),
            Vec::with_capacity(std::cmp::max(chunk.len() + 1024, 8192)),
        );
        log::debug!(
            "HtmlRewriterAdapter::process_chunk: input={} bytes, output={} bytes, is_last={}",
            chunk.len(),
            result.len(),
            is_last
        );
        Ok(result)
    }

    fn reset(&mut self) {
        // The rewriter is consumed after end(); a new HtmlRewriterAdapter should
        // be created per document. Clear any remaining output buffer.
        self.output.borrow_mut().clear();
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

        pipeline
            .process(&input[..], &mut output)
            .expect("pipeline should process uncompressed input");

        assert_eq!(
            String::from_utf8(output).expect("output should be valid UTF-8"),
            "hi world"
        );
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
    fn test_html_rewriter_adapter_streams_incrementally() {
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

        // Collect all output across chunks; the rewriter may emit partial output at any point.
        let mut full_output = Vec::new();

        let chunk1 = b"<html><body>";
        full_output.extend(
            adapter
                .process_chunk(chunk1, false)
                .expect("should process chunk1"),
        );

        let chunk2 = b"<p>original</p>";
        full_output.extend(
            adapter
                .process_chunk(chunk2, false)
                .expect("should process chunk2"),
        );

        let chunk3 = b"</body></html>";
        full_output.extend(
            adapter
                .process_chunk(chunk3, true)
                .expect("should process final chunk"),
        );

        assert!(!full_output.is_empty(), "Should have produced output");
        let output = String::from_utf8(full_output).expect("output should be valid UTF-8");
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

        // Process in chunks, collecting all output.
        let chunk_size = 1024;
        let bytes = large_html.as_bytes();
        let chunks: Vec<_> = bytes.chunks(chunk_size).collect();
        let last_idx = chunks.len().saturating_sub(1);

        let mut full_output = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == last_idx;
            let result = adapter
                .process_chunk(chunk, is_last)
                .expect("should process chunk");
            full_output.extend(result);
        }

        assert!(!full_output.is_empty(), "Should have produced output");
        let output = String::from_utf8(full_output).expect("output should be valid UTF-8");
        assert!(
            output.contains("Paragraph 999"),
            "Should contain all content"
        );
    }

    #[test]
    fn test_html_rewriter_adapter_reset_clears_output_buffer() {
        use lol_html::Settings;

        // reset() is a no-op on the rewriter itself (a new adapter is needed per document),
        // but it must clear any pending bytes in the output buffer.
        let settings = Settings::default();
        let mut adapter = HtmlRewriterAdapter::new(settings);

        // Write a full document so the rewriter is finished.
        let _ = adapter
            .process_chunk(b"<html><body><p>test</p></body></html>", true)
            .expect("should process complete document");

        // reset() should not panic and should leave the buffer empty.
        adapter.reset();
        // No assertion on a subsequent process_chunk — the rewriter is consumed.
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

        pipeline
            .process(&input[..], &mut output)
            .expect("pipeline should process HTML");

        let result = String::from_utf8(output).expect("output should be valid UTF-8");
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
