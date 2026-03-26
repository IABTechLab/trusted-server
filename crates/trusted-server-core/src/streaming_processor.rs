//! Unified streaming processor architecture for handling compressed and uncompressed content.
//!
//! This module provides a flexible pipeline for processing content streams with:
//! - Automatic compression/decompression handling
//! - Pluggable content processors (text replacement, HTML rewriting, etc.)
//! - Memory-efficient streaming
//! - UTF-8 boundary handling

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;

use error_stack::{Report, ResultExt};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Configuration for the streaming pipeline.
///
/// # Supported compression combinations
///
/// | Input | Output | Behavior |
/// |-------|--------|----------|
/// | None | None | Pass-through processing |
/// | Gzip | Gzip | Decompress → process → recompress |
/// | Gzip | None | Decompress → process |
/// | Deflate | Deflate | Decompress → process → recompress |
/// | Deflate | None | Decompress → process |
/// | Brotli | Brotli | Decompress → process → recompress |
/// | Brotli | None | Decompress → process |
///
/// All other combinations return an error at runtime.
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
    /// Handles all supported compression transformations by wrapping the raw
    /// reader/writer in the appropriate decoder/encoder, then delegating to
    /// [`Self::process_chunks`].
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
            (Compression::None, Compression::None) => self.process_chunks(input, output),
            (Compression::Gzip, Compression::Gzip) => {
                use flate2::read::GzDecoder;
                use flate2::write::GzEncoder;

                let decoder = GzDecoder::new(input);
                let mut encoder = GzEncoder::new(output, flate2::Compression::default());
                self.process_chunks(decoder, &mut encoder)?;
                encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize gzip encoder".to_string(),
                })?;
                Ok(())
            }
            (Compression::Gzip, Compression::None) => {
                use flate2::read::GzDecoder;

                self.process_chunks(GzDecoder::new(input), output)
            }
            (Compression::Deflate, Compression::Deflate) => {
                use flate2::read::ZlibDecoder;
                use flate2::write::ZlibEncoder;

                let decoder = ZlibDecoder::new(input);
                let mut encoder = ZlibEncoder::new(output, flate2::Compression::default());
                self.process_chunks(decoder, &mut encoder)?;
                encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize deflate encoder".to_string(),
                })?;
                Ok(())
            }
            (Compression::Deflate, Compression::None) => {
                use flate2::read::ZlibDecoder;

                self.process_chunks(ZlibDecoder::new(input), output)
            }
            (Compression::Brotli, Compression::Brotli) => {
                use brotli::enc::writer::CompressorWriter;
                use brotli::enc::BrotliEncoderParams;
                use brotli::Decompressor;

                let decoder = Decompressor::new(input, 4096);
                let params = BrotliEncoderParams {
                    quality: 4,
                    lgwin: 22,
                    ..Default::default()
                };
                let mut encoder = CompressorWriter::with_params(output, 4096, &params);
                self.process_chunks(decoder, &mut encoder)?;
                // CompressorWriter emits the brotli stream trailer via flush(),
                // which process_chunks already called. into_inner() avoids a
                // redundant flush on drop and makes finalization explicit.
                // Note: unlike flate2's finish(), CompressorWriter has no
                // fallible finalization method — flush() is the only option.
                let _ = encoder.into_inner();
                Ok(())
            }
            (Compression::Brotli, Compression::None) => {
                use brotli::Decompressor;

                self.process_chunks(Decompressor::new(input, 4096), output)
            }
            _ => Err(Report::new(TrustedServerError::Proxy {
                message: "Unsupported compression transformation".to_string(),
            })),
        }
    }

    /// Read chunks from `reader`, pass each through the processor, and write output to `writer`.
    ///
    /// This is the single unified chunk loop used by all compression paths.
    /// The method calls `writer.flush()` before returning. For the `None → None`
    /// path this is the only finalization needed. For compressed paths, the caller
    /// must still call the encoder's type-specific finalization (e.g., `finish()`
    /// for flate2, `into_inner()` for brotli) — `flush()` alone does not write
    /// compression trailers for all codecs.
    ///
    /// # Errors
    ///
    /// Returns an error if reading, processing, or writing any chunk fails.
    fn process_chunks<R: Read, W: Write>(
        &mut self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut buffer = vec![0u8; self.config.chunk_size];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let final_chunk = self.processor.process_chunk(&[], true).change_context(
                        TrustedServerError::Proxy {
                            message: "Failed to process final chunk".to_string(),
                        },
                    )?;
                    if !final_chunk.is_empty() {
                        writer.write_all(&final_chunk).change_context(
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
                        writer
                            .write_all(&processed)
                            .change_context(TrustedServerError::Proxy {
                                message: "Failed to write processed chunk".to_string(),
                            })?;
                    }
                }
                Err(e) => {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: format!("Failed to read: {e}"),
                    }));
                }
            }
        }

        writer.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush output".to_string(),
        })?;

        Ok(())
    }
}

/// Shared output buffer used as an [`lol_html::OutputSink`].
///
/// The `HtmlRewriter` invokes [`OutputSink::handle_chunk`] synchronously during
/// each [`HtmlRewriter::write`] call, so the buffer is drained after every
/// `process_chunk` invocation to emit output incrementally.
struct RcVecSink(Rc<RefCell<Vec<u8>>>);

impl lol_html::OutputSink for RcVecSink {
    fn handle_chunk(&mut self, chunk: &[u8]) {
        self.0.borrow_mut().extend_from_slice(chunk);
    }
}

/// Adapter to use `lol_html` [`HtmlRewriter`](lol_html::HtmlRewriter) as a [`StreamProcessor`].
///
/// Output is emitted incrementally on every [`StreamProcessor::process_chunk`] call.
/// The adapter is single-use: one adapter per request. Calling [`StreamProcessor::reset`]
/// is a no-op because the rewriter consumes its settings on construction.
pub struct HtmlRewriterAdapter {
    rewriter: Option<lol_html::HtmlRewriter<'static, RcVecSink>>,
    output: Rc<RefCell<Vec<u8>>>,
}

impl HtmlRewriterAdapter {
    /// Create a new HTML rewriter adapter that streams output per chunk.
    #[must_use]
    pub fn new(settings: lol_html::Settings<'static, 'static>) -> Self {
        let output = Rc::new(RefCell::new(Vec::new()));
        let sink = RcVecSink(Rc::clone(&output));
        let rewriter = lol_html::HtmlRewriter::new(settings, sink);
        Self {
            rewriter: Some(rewriter),
            output,
        }
    }
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        match &mut self.rewriter {
            Some(rewriter) => {
                if !chunk.is_empty() {
                    rewriter.write(chunk).map_err(|e| {
                        log::error!("Failed to process HTML chunk: {e}");
                        io::Error::other(format!("HTML processing failed: {e}"))
                    })?;
                }
            }
            None if !chunk.is_empty() => {
                log::warn!(
                    "HtmlRewriterAdapter: {} bytes received after finalization, data will be lost",
                    chunk.len()
                );
            }
            None => {}
        }

        if is_last {
            if let Some(rewriter) = self.rewriter.take() {
                rewriter.end().map_err(|e| {
                    log::error!("Failed to finalize HTML: {e}");
                    io::Error::other(format!("HTML finalization failed: {e}"))
                })?;
            }
        }

        // Drain whatever lol_html produced since the last call
        Ok(std::mem::take(&mut *self.output.borrow_mut()))
    }

    /// No-op. `HtmlRewriterAdapter` is single-use: the rewriter consumes its
    /// [`Settings`](lol_html::Settings) on construction and cannot be recreated.
    /// Calling [`process_chunk`](StreamProcessor::process_chunk) after
    /// [`process_chunk`](StreamProcessor::process_chunk) with `is_last = true`
    /// will produce empty output.
    fn reset(&mut self) {}
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

        let chunk1 = b"<html><body>";
        let result1 = adapter
            .process_chunk(chunk1, false)
            .expect("should process chunk1");

        let chunk2 = b"<p>original</p>";
        let result2 = adapter
            .process_chunk(chunk2, false)
            .expect("should process chunk2");

        let chunk3 = b"</body></html>";
        let result3 = adapter
            .process_chunk(chunk3, true)
            .expect("should process final chunk");

        // Concatenate all outputs and verify the final HTML is correct
        let mut all_output = result1;
        all_output.extend_from_slice(&result2);
        all_output.extend_from_slice(&result3);

        assert!(
            !all_output.is_empty(),
            "should produce non-empty concatenated output"
        );

        let output = String::from_utf8(all_output).expect("output should be valid UTF-8");
        assert!(
            output.contains("replaced"),
            "should have replaced content in concatenated output"
        );
        assert!(
            output.contains("<html>"),
            "should have complete HTML in concatenated output"
        );
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

        // Process in chunks and collect all output
        let chunk_size = 1024;
        let bytes = large_html.as_bytes();
        let mut chunks = bytes.chunks(chunk_size).peekable();
        let mut all_output = Vec::new();

        while let Some(chunk) = chunks.next() {
            let is_last = chunks.peek().is_none();
            let result = adapter
                .process_chunk(chunk, is_last)
                .expect("should process chunk");
            all_output.extend_from_slice(&result);
        }

        assert!(
            !all_output.is_empty(),
            "should produce non-empty output for large document"
        );

        let output = String::from_utf8(all_output).expect("output should be valid UTF-8");
        assert!(
            output.contains("Paragraph 999"),
            "should contain all content from large document"
        );
    }

    #[test]
    fn test_html_rewriter_adapter_reset_then_finalize() {
        use lol_html::Settings;

        let settings = Settings::default();
        let mut adapter = HtmlRewriterAdapter::new(settings);

        let result1 = adapter
            .process_chunk(b"<html><body>test</body></html>", false)
            .expect("should process html");

        // reset() is a documented no-op — adapter is single-use
        adapter.reset();

        // Finalize still works; the rewriter is still alive
        let result2 = adapter
            .process_chunk(b"", true)
            .expect("should finalize after reset");

        let mut all_output = result1;
        all_output.extend_from_slice(&result2);
        let output = String::from_utf8(all_output).expect("output should be valid UTF-8");
        assert!(
            output.contains("test"),
            "should produce correct output despite no-op reset"
        );
    }

    #[test]
    fn test_deflate_round_trip_produces_valid_output() {
        // Verify that deflate-to-deflate produces valid output that decompresses
        // correctly, confirming that encoder finalization works.
        use flate2::read::ZlibDecoder;
        use flate2::write::ZlibEncoder;
        use std::io::{Read as _, Write as _};

        let input_data = b"<html><body>hello world</body></html>";

        // Compress input
        let mut compressed_input = Vec::new();
        {
            let mut enc = ZlibEncoder::new(&mut compressed_input, flate2::Compression::default());
            enc.write_all(input_data)
                .expect("should compress test input");
            enc.finish().expect("should finish compression");
        }

        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_string(),
            replace_with: "hi".to_string(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Deflate,
            output_compression: Compression::Deflate,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        pipeline
            .process(&compressed_input[..], &mut output)
            .expect("should process deflate-to-deflate");

        // Decompress output and verify correctness
        let mut decompressed = Vec::new();
        ZlibDecoder::new(&output[..])
            .read_to_end(&mut decompressed)
            .expect("should decompress output — implies encoder was finalized correctly");

        assert_eq!(
            String::from_utf8(decompressed).expect("should be valid UTF-8"),
            "<html><body>hi world</body></html>",
            "should have replaced content through deflate round-trip"
        );
    }

    #[test]
    fn test_gzip_to_gzip_produces_correct_output() {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use std::io::{Read as _, Write as _};

        // Arrange
        let input_data = b"<html><body>hello world</body></html>";

        let mut compressed_input = Vec::new();
        {
            let mut enc = GzEncoder::new(&mut compressed_input, flate2::Compression::default());
            enc.write_all(input_data)
                .expect("should compress test input");
            enc.finish().expect("should finish compression");
        }

        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_string(),
            replace_with: "hi".to_string(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        // Act
        pipeline
            .process(&compressed_input[..], &mut output)
            .expect("should process gzip-to-gzip");

        // Assert
        let mut decompressed = Vec::new();
        GzDecoder::new(&output[..])
            .read_to_end(&mut decompressed)
            .expect("should decompress output — implies encoder was finalized correctly");

        assert_eq!(
            String::from_utf8(decompressed).expect("should be valid UTF-8"),
            "<html><body>hi world</body></html>",
            "should have replaced content through gzip round-trip"
        );
    }

    #[test]
    fn test_gzip_to_none_produces_correct_output() {
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        // Arrange
        let input_data = b"<html><body>hello world</body></html>";

        let mut compressed_input = Vec::new();
        {
            let mut enc = GzEncoder::new(&mut compressed_input, flate2::Compression::default());
            enc.write_all(input_data)
                .expect("should compress test input");
            enc.finish().expect("should finish compression");
        }

        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_string(),
            replace_with: "hi".to_string(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Gzip,
            output_compression: Compression::None,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        // Act
        pipeline
            .process(&compressed_input[..], &mut output)
            .expect("should process gzip-to-none");

        // Assert
        let result = String::from_utf8(output).expect("should be valid UTF-8 uncompressed output");
        assert_eq!(
            result, "<html><body>hi world</body></html>",
            "should have replaced content after gzip decompression"
        );
    }

    #[test]
    fn test_brotli_round_trip_produces_valid_output() {
        use brotli::enc::writer::CompressorWriter;
        use brotli::Decompressor;
        use std::io::{Read as _, Write as _};

        let input_data = b"<html><body>hello world</body></html>";

        // Compress input with brotli
        let mut compressed_input = Vec::new();
        {
            let mut enc = CompressorWriter::new(&mut compressed_input, 4096, 4, 22);
            enc.write_all(input_data)
                .expect("should compress test input");
            enc.flush().expect("should flush brotli encoder");
        }

        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_string(),
            replace_with: "hi".to_string(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Brotli,
            output_compression: Compression::Brotli,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        pipeline
            .process(&compressed_input[..], &mut output)
            .expect("should process brotli-to-brotli");

        // Decompress output and verify correctness
        let mut decompressed = Vec::new();
        Decompressor::new(&output[..], 4096)
            .read_to_end(&mut decompressed)
            .expect("should decompress output — implies encoder was finalized correctly");

        assert_eq!(
            String::from_utf8(decompressed).expect("should be valid UTF-8"),
            "<html><body>hi world</body></html>",
            "should have replaced content through brotli round-trip"
        );
    }

    #[test]
    fn test_html_rewriter_adapter_emits_output_per_chunk() {
        use lol_html::Settings;

        let settings = Settings::default();
        let mut adapter = HtmlRewriterAdapter::new(settings);

        // Send three chunks — lol_html may buffer internally, so individual
        // chunk outputs may vary by version. The contract is that concatenated
        // output is correct, and that output is not deferred entirely to is_last.
        let result1 = adapter
            .process_chunk(b"<html><body>", false)
            .expect("should process chunk1");
        let result2 = adapter
            .process_chunk(b"<p>hello</p>", false)
            .expect("should process chunk2");
        let result3 = adapter
            .process_chunk(b"</body></html>", true)
            .expect("should process final chunk");

        // At least one intermediate chunk should produce output (verifies
        // we're not deferring everything to is_last like the old adapter).
        assert!(
            !result1.is_empty() || !result2.is_empty(),
            "should emit some output before is_last"
        );

        // Concatenated output must be correct
        let mut all_output = result1;
        all_output.extend_from_slice(&result2);
        all_output.extend_from_slice(&result3);

        let output = String::from_utf8(all_output).expect("output should be valid UTF-8");
        assert!(
            output.contains("<html>"),
            "should contain html tag in concatenated output"
        );
        assert!(
            output.contains("<p>hello</p>"),
            "should contain paragraph in concatenated output"
        );
        assert!(
            output.contains("</html>"),
            "should contain closing html tag in concatenated output"
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

    #[test]
    fn test_gzip_pipeline_with_html_rewriter() {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use lol_html::{element, Settings};
        use std::io::{Read as _, Write as _};

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

        let input = b"<html><body><a href=\"https://example.com\">Link</a></body></html>";

        let mut compressed_input = Vec::new();
        {
            let mut enc = GzEncoder::new(&mut compressed_input, flate2::Compression::default());
            enc.write_all(input).expect("should compress test input");
            enc.finish().expect("should finish compression");
        }

        let adapter = HtmlRewriterAdapter::new(settings);
        let config = PipelineConfig {
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(config, adapter);
        let mut output = Vec::new();

        pipeline
            .process(&compressed_input[..], &mut output)
            .expect("pipeline should process gzip HTML");

        let mut decompressed = Vec::new();
        GzDecoder::new(&output[..])
            .read_to_end(&mut decompressed)
            .expect("should decompress output");

        let result = String::from_utf8(decompressed).expect("output should be valid UTF-8");
        assert!(
            result.contains("https://test.com"),
            "should have replaced URL through gzip HTML pipeline"
        );
        assert!(
            !result.contains("example.com"),
            "should not contain original URL after gzip HTML pipeline"
        );
    }
}
