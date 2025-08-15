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
    pub fn new(config: PipelineConfig, processor: P) -> Self {
        Self { config, processor }
    }

    /// Process a stream from input to output
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
            (Compression::Deflate, Compression::Deflate) => {
                self.process_deflate_to_deflate(input, output)
            }
            (Compression::Brotli, Compression::Brotli) => {
                self.process_brotli_to_brotli(input, output)
            }
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

        let decoder = GzDecoder::new(input);
        let encoder = GzEncoder::new(output, Compression::default());

        self.process_through_compression(decoder, encoder)
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

        Ok(())
    }
}

/// Adapter to use lol_html HtmlRewriter as a StreamProcessor
/// Note: Due to lol_html's design, this accumulates output and returns it all at once
pub struct HtmlRewriterAdapter {
    settings: lol_html::Settings<'static, 'static>,
    accumulated_input: Vec<u8>,
}

impl HtmlRewriterAdapter {
    /// Create a new HTML rewriter adapter
    pub fn new(settings: lol_html::Settings<'static, 'static>) -> Self {
        Self {
            settings,
            accumulated_input: Vec::new(),
        }
    }
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        // Accumulate input
        self.accumulated_input.extend_from_slice(chunk);
        
        // Log accumulation progress
        if chunk.len() > 0 {
            log::debug!("[HtmlRewriter] Accumulated {} bytes, total: {} bytes", 
                chunk.len(), self.accumulated_input.len());
        }

        if is_last {
            log::info!("[HtmlRewriter] Processing final chunk, total size: {} bytes", 
                self.accumulated_input.len());
            
            // Process all accumulated input
            let mut output = Vec::new();

            {
                let mut rewriter = lol_html::HtmlRewriter::new(
                    std::mem::take(&mut self.settings),
                    |chunk: &[u8]| {
                        output.extend_from_slice(chunk);
                    },
                );

                rewriter
                    .write(&self.accumulated_input)
                    .map_err(|e| {
                        log::error!("[HtmlRewriter] Write failed: {}", e);
                        io::Error::other(format!("HTML rewriter write failed: {}", e))
                    })?;

                rewriter
                    .end()
                    .map_err(|e| {
                        log::error!("[HtmlRewriter] End failed: {}", e);
                        io::Error::other(format!("HTML rewriter end failed: {}", e))
                    })?;
            }

            log::info!("[HtmlRewriter] Processed output size: {} bytes", output.len());
            self.accumulated_input.clear();
            Ok(output)
        } else {
            // Accumulate more input
            Ok(Vec::new())
        }
    }

    fn reset(&mut self) {
        self.accumulated_input.clear();
    }
}

/// Adapter to use our existing StreamingReplacer as a StreamProcessor
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
}
