//! Unified streaming processor architecture for handling compressed and uncompressed content.
//!
//! This module provides a flexible pipeline for processing content streams with:
//! - Automatic compression/decompression handling
//! - Pluggable content processors (text replacement, HTML rewriting, etc.)
//! - Memory-efficient streaming
//! - UTF-8 boundary handling
//!
//! # Platform notes
//!
//! This module is **platform-agnostic** (verified 2026-03-31; see
//! `docs/superpowers/plans/2026-03-31-pr8-content-rewriting-verification.md`). It has zero
//! `fastly` imports. [`StreamingPipeline::process`] is generic over
//! `R: Read + W: Write` — any reader or writer works, including
//! any platform body type (which implements `std::io::Read`) or standard
//! `std::io::Cursor<&[u8]>`.
//!
//! Future adapters (Cloudflare Workers, Axum, Spin) do not need to implement any compression or
//! streaming interface. See `crate::platform` module doc for the
//! authoritative note.

use std::cell::{Cell, RefCell};
use std::io::{self, Read, Write};
use std::rc::Rc;

use brotli::Decompressor;
use brotli::enc::BrotliEncoderParams;
use brotli::enc::writer::CompressorWriter;
use error_stack::{Report, ResultExt as _};
use flate2::read::{MultiGzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};

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
        match encoding {
            s if s.eq_ignore_ascii_case("gzip") => Self::Gzip,
            s if s.eq_ignore_ascii_case("deflate") => Self::Deflate,
            s if s.eq_ignore_ascii_case("br") => Self::Brotli,
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
    /// `Self::process_chunks`.
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
                // Multi-member decoder: RFC 1952 permits concatenated gzip
                // members, so a single-member reader would stop after the first.
                // Matches the streaming `BodyStreamDecoder` gzip codec.
                let decoder = MultiGzDecoder::new(input);
                let mut encoder = GzEncoder::new(output, flate2::Compression::default());
                self.process_chunks(decoder, &mut encoder)?;
                encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize gzip encoder".to_owned(),
                })?;
                Ok(())
            }
            (Compression::Gzip, Compression::None) => {
                self.process_chunks(MultiGzDecoder::new(input), output)
            }
            (Compression::Deflate, Compression::Deflate) => {
                let decoder = ZlibDecoder::new(input);
                let mut encoder = ZlibEncoder::new(output, flate2::Compression::default());
                self.process_chunks(decoder, &mut encoder)?;
                encoder.finish().change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize deflate encoder".to_owned(),
                })?;
                Ok(())
            }
            (Compression::Deflate, Compression::None) => {
                self.process_chunks(ZlibDecoder::new(input), output)
            }
            (Compression::Brotli, Compression::Brotli) => {
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
                self.process_chunks(Decompressor::new(input, 4096), output)
            }
            _ => Err(Report::new(TrustedServerError::Proxy {
                message: "Unsupported compression transformation".to_owned(),
            })),
        }
    }

    /// Read chunks from `reader`, pass each through the processor, and write output to `writer`.
    ///
    /// This is the single unified chunk loop used by all compression paths.
    /// The method calls `writer.flush()` before returning. For the `None → None`
    /// path this is the only finalization needed. For compressed paths, the caller
    /// must still call the encoder's type-specific finalization after this returns:
    /// - **flate2** (`GzEncoder`, `ZlibEncoder`): call `finish()` — `flush()` does
    ///   not write the gzip/deflate trailer.
    /// - **brotli** (`CompressorWriter`): `flush()` does finalize the stream, so
    ///   the caller only needs `into_inner()` to reclaim the writer.
    ///
    /// # Errors
    ///
    /// Returns an error if reading, processing, or writing any chunk fails.
    fn process_chunks<R: Read, W: Write>(
        &mut self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut buffer = vec![0_u8; self.config.chunk_size];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let final_chunk = self.processor.process_chunk(&[], true).change_context(
                        TrustedServerError::Proxy {
                            message: "Failed to process final chunk".to_owned(),
                        },
                    )?;
                    if !final_chunk.is_empty() {
                        writer.write_all(&final_chunk).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write final chunk".to_owned(),
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
                            message: "Failed to process chunk".to_owned(),
                        })?;
                    if !processed.is_empty() {
                        writer
                            .write_all(&processed)
                            .change_context(TrustedServerError::Proxy {
                                message: "Failed to write processed chunk".to_owned(),
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
            message: "Failed to flush output".to_owned(),
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
/// Output is emitted incrementally on every [`process_chunk`](StreamProcessor::process_chunk)
/// call. Script rewriters that receive text from `lol_html` must be fragment-safe —
/// they accumulate text fragments internally until `is_last_in_text_node` is true.
///
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
        match (&mut self.rewriter, chunk.is_empty()) {
            (Some(rewriter), false) => {
                rewriter.write(chunk).map_err(|e| {
                    log::error!("Failed to process HTML chunk: {e}");
                    io::Error::other(format!("HTML processing failed: {e}"))
                })?;
            }
            (None, false) => {
                log::warn!(
                    "HtmlRewriterAdapter: {} bytes received after finalization, data will be lost",
                    chunk.len()
                );
            }
            _ => {}
        }

        if is_last && let Some(rewriter) = self.rewriter.take() {
            rewriter.end().map_err(|e| {
                log::error!("Failed to finalize HTML: {e}");
                io::Error::other(format!("HTML finalization failed: {e}"))
            })?;
        }

        // Drain whatever lol_html produced since the last call
        Ok(std::mem::take(&mut *self.output.borrow_mut()))
    }

    /// No-op. `HtmlRewriterAdapter` is single-use: the rewriter consumes its
    /// [`Settings`](lol_html::Settings) on construction and cannot be recreated.
    /// Calling [`process_chunk`](StreamProcessor::process_chunk) after finalization
    /// (`is_last = true`) will produce empty output — the rewriter is already done.
    fn reset(&mut self) {}
}

/// Adapter to use our existing `StreamingReplacer` as a `StreamProcessor`
use crate::streaming_replacer::StreamingReplacer;

impl StreamProcessor for StreamingReplacer {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        Ok(self.process_chunk(chunk, is_last))
    }
}

/// Read buffer size for streaming body processing and brotli internal buffers.
/// Both the `Decompressor` and `CompressorWriter` use this value so all
/// brotli I/O layers operate on consistently-sized chunks.
pub(crate) const STREAM_CHUNK_SIZE: usize = 8192;

/// Incremental push-style decompressor for the async chunk pipeline.
///
/// Compressed bytes go in via [`Self::decode_chunk`]; decoded bytes drain
/// out of the internal buffer after every push. Write-based decoders are
/// used because the async publisher path cannot wrap a blocking `Read`.
///
/// Decoded output is capped cumulatively and the cap is enforced *during*
/// decompression, not after: the chunk source only bounds raw (still
/// compressed) bytes, and a decompression bomb can expand ~1000x past that, so
/// a small compressed chunk must not be allowed to fully expand before the
/// ceiling is checked. The gzip and brotli codecs decode into a
/// [`BoundedDecodeSink`] that errors the moment a write would exceed the limit;
/// the deflate codec charges each produced output block as it is emitted.
///
/// Every codec validates end-of-stream at [`Self::finish`] so a truncated
/// origin body errors instead of silently truncating the page: gzip via its
/// trailer checksum, brotli via `close()`, and deflate by driving
/// [`flate2::Decompress`] to its [`flate2::Status::StreamEnd`] marker (the
/// `write`-based zlib decoder accepts truncated input silently, so the deflate
/// arm drives [`flate2::Decompress`] directly). Concatenated gzip members
/// (RFC 1952) are decoded via [`flate2::write::MultiGzDecoder`].
pub(crate) struct BodyStreamDecoder {
    codec: BodyStreamDecoderCodec,
    /// Cumulative decoded byte count, shared with the codec sinks so the cap is
    /// enforced from inside the decompressor writes rather than after them.
    decoded_bytes: Rc<Cell<usize>>,
    max_decoded_bytes: usize,
}

enum BodyStreamDecoderCodec {
    None,
    Gzip(flate2::write::MultiGzDecoder<BoundedDecodeSink>),
    Deflate(DeflateStreamDecoder),
    Brotli(Box<brotli::DecompressorWriter<BoundedDecodeSink>>),
}

/// A [`Write`] sink that buffers decoded bytes while enforcing a shared
/// cumulative decode budget.
///
/// The gzip and brotli decoders write their decompressed output here as they
/// process input. Rejecting the write as soon as it would push the cumulative
/// decoded total past `max_decoded_bytes` makes the cap a hard ceiling on
/// Wasm-heap growth: a decompression bomb errors before its expanded bytes are
/// buffered, rather than after a full chunk has already expanded.
struct BoundedDecodeSink {
    buffer: Vec<u8>,
    decoded_bytes: Rc<Cell<usize>>,
    max_decoded_bytes: usize,
}

impl BoundedDecodeSink {
    fn new(decoded_bytes: Rc<Cell<usize>>, max_decoded_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            decoded_bytes,
            max_decoded_bytes,
        }
    }
}

impl Write for BoundedDecodeSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let next = self
            .decoded_bytes
            .get()
            .checked_add(data.len())
            .ok_or_else(|| {
                io::Error::other("publisher origin body decoded byte count overflowed")
            })?;
        if next > self.max_decoded_bytes {
            return Err(io::Error::other(format!(
                "publisher origin body decoded size exceeded {}-byte streaming limit",
                self.max_decoded_bytes
            )));
        }
        self.decoded_bytes.set(next);
        self.buffer.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Charge `len` decoded bytes against `decoded_bytes`, erroring if the
/// cumulative total would exceed `max_decoded_bytes`.
fn charge_decoded(
    decoded_bytes: &Cell<usize>,
    max_decoded_bytes: usize,
    len: usize,
) -> Result<(), Report<TrustedServerError>> {
    let next = decoded_bytes.get().checked_add(len).ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "publisher origin body decoded byte count overflowed".to_string(),
        })
    })?;
    if next > max_decoded_bytes {
        return Err(Report::new(TrustedServerError::Proxy {
            message: format!(
                "publisher origin body decoded size exceeded {max_decoded_bytes}-byte streaming limit"
            ),
        }));
    }
    decoded_bytes.set(next);
    Ok(())
}

/// Streaming zlib decoder that tracks whether the stream reached its end
/// marker, so truncated deflate bodies fail at finalization.
struct DeflateStreamDecoder {
    decompress: flate2::Decompress,
    stream_ended: bool,
    decoded_bytes: Rc<Cell<usize>>,
    max_decoded_bytes: usize,
}

impl DeflateStreamDecoder {
    fn new(decoded_bytes: Rc<Cell<usize>>, max_decoded_bytes: usize) -> Self {
        Self {
            decompress: flate2::Decompress::new(true),
            stream_ended: false,
            decoded_bytes,
            max_decoded_bytes,
        }
    }

    /// Charge `len` decoded bytes against the shared budget.
    fn charge(&self, len: usize) -> Result<(), Report<TrustedServerError>> {
        charge_decoded(&self.decoded_bytes, self.max_decoded_bytes, len)
    }

    /// Decode as much of `chunk` as possible, draining any output the inflater
    /// can still produce once all input is consumed.
    ///
    /// flate2 fills the output buffer up to its capacity, so a chunk that
    /// exactly fills the buffer leaves decoded bytes (and possibly the
    /// end-of-stream marker) pending with all input already consumed. The loop
    /// keeps driving the inflater — reserving more output space — until it
    /// makes no further progress, so those pending bytes are never stranded and
    /// a valid stream is not mistaken for a truncated one at `finish`.
    fn decode(&mut self, chunk: &[u8]) -> Result<Vec<u8>, Report<TrustedServerError>> {
        let mut output = Vec::with_capacity(STREAM_CHUNK_SIZE);
        let mut offset = 0usize;
        // Trailing bytes after the zlib end marker are ignored, matching the
        // read-based decoder used by the buffered pipeline.
        while !self.stream_ended {
            if output.len() == output.capacity() {
                output.reserve(STREAM_CHUNK_SIZE);
            }
            let before_in = self.decompress.total_in();
            let before_out = self.decompress.total_out();
            let status = self
                .decompress
                .decompress_vec(&chunk[offset..], &mut output, flate2::FlushDecompress::None)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to decode deflate publisher body chunk".to_string(),
                })?;
            let consumed = (self.decompress.total_in() - before_in) as usize;
            let produced = (self.decompress.total_out() - before_out) as usize;
            offset += consumed;
            self.charge(produced)?;
            match status {
                flate2::Status::StreamEnd => self.stream_ended = true,
                flate2::Status::Ok | flate2::Status::BufError => {
                    // Stop only when the inflater is starved for input: it made
                    // no progress and there is still spare output capacity, so
                    // the stall is missing input (arriving in a later chunk, or
                    // resolved at `finish`), not an exhausted output buffer.
                    if consumed == 0 && produced == 0 && output.len() < output.capacity() {
                        break;
                    }
                }
            }
        }
        Ok(output)
    }

    /// Drive the inflater to completion at end of input, draining the final
    /// decoded bytes and validating the end-of-stream marker.
    ///
    /// A valid stream whose last decoded byte exactly filled the previous
    /// output buffer still has its end marker pending here; a genuinely
    /// truncated stream makes no further progress and errors.
    fn finish(&mut self) -> Result<Vec<u8>, Report<TrustedServerError>> {
        let mut output = Vec::new();
        while !self.stream_ended {
            if output.len() == output.capacity() {
                output.reserve(STREAM_CHUNK_SIZE);
            }
            let before_out = self.decompress.total_out();
            let status = self
                .decompress
                .decompress_vec(&[], &mut output, flate2::FlushDecompress::Finish)
                .change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize deflate publisher body decoder".to_string(),
                })?;
            let produced = (self.decompress.total_out() - before_out) as usize;
            self.charge(produced)?;
            match status {
                flate2::Status::StreamEnd => self.stream_ended = true,
                flate2::Status::Ok | flate2::Status::BufError => {
                    if produced == 0 {
                        break;
                    }
                }
            }
        }
        if !self.stream_ended {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "Failed to finalize deflate publisher body decoder: truncated stream"
                    .to_string(),
            }));
        }
        Ok(output)
    }
}

impl BodyStreamDecoder {
    pub(crate) fn new(compression: Compression, max_decoded_bytes: usize) -> Self {
        let decoded_bytes = Rc::new(Cell::new(0usize));
        let codec = match compression {
            Compression::None => BodyStreamDecoderCodec::None,
            Compression::Gzip => BodyStreamDecoderCodec::Gzip(flate2::write::MultiGzDecoder::new(
                BoundedDecodeSink::new(Rc::clone(&decoded_bytes), max_decoded_bytes),
            )),
            Compression::Deflate => BodyStreamDecoderCodec::Deflate(DeflateStreamDecoder::new(
                Rc::clone(&decoded_bytes),
                max_decoded_bytes,
            )),
            Compression::Brotli => {
                BodyStreamDecoderCodec::Brotli(Box::new(brotli::DecompressorWriter::new(
                    BoundedDecodeSink::new(Rc::clone(&decoded_bytes), max_decoded_bytes),
                    STREAM_CHUNK_SIZE,
                )))
            }
        };
        Self {
            codec,
            decoded_bytes,
            max_decoded_bytes,
        }
    }

    pub(crate) fn decode_chunk(
        &mut self,
        chunk: bytes::Bytes,
    ) -> Result<bytes::Bytes, Report<TrustedServerError>> {
        match &mut self.codec {
            BodyStreamDecoderCodec::None => {
                // No sink guards the pass-through path, so charge the raw chunk
                // directly against the shared budget.
                charge_decoded(&self.decoded_bytes, self.max_decoded_bytes, chunk.len())?;
                Ok(chunk)
            }
            BodyStreamDecoderCodec::Gzip(decoder) => {
                decoder
                    .write_all(&chunk)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to decode gzip publisher body chunk".to_string(),
                    })?;
                // The sink charged the decoded bytes during `write_all`.
                Ok(bytes::Bytes::from(std::mem::take(
                    &mut decoder.get_mut().buffer,
                )))
            }
            BodyStreamDecoderCodec::Deflate(decoder) => {
                Ok(bytes::Bytes::from(decoder.decode(&chunk)?))
            }
            BodyStreamDecoderCodec::Brotli(decoder) => {
                decoder
                    .write_all(&chunk)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to decode brotli publisher body chunk".to_string(),
                    })?;
                Ok(bytes::Bytes::from(std::mem::take(
                    &mut decoder.get_mut().buffer,
                )))
            }
        }
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<u8>, Report<TrustedServerError>> {
        match &mut self.codec {
            BodyStreamDecoderCodec::None => Ok(Vec::new()),
            BodyStreamDecoderCodec::Gzip(decoder) => {
                decoder
                    .try_finish()
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to finalize gzip publisher body decoder".to_string(),
                    })?;
                Ok(std::mem::take(&mut decoder.get_mut().buffer))
            }
            BodyStreamDecoderCodec::Deflate(decoder) => decoder.finish(),
            BodyStreamDecoderCodec::Brotli(decoder) => {
                // `close()` (not `flush()`): flush accepts a truncated brotli
                // stream silently, while close validates end-of-stream and
                // errors on incomplete input, matching the gzip/deflate arms.
                decoder.close().change_context(TrustedServerError::Proxy {
                    message: "Failed to finalize brotli publisher body decoder".to_string(),
                })?;
                Ok(std::mem::take(&mut decoder.get_mut().buffer))
            }
        }
    }
}

/// Incremental push-style compressor mirroring [`BodyStreamDecoder`].
///
/// Processed bytes go in via [`Self::encode_chunk`]; encoded bytes drain out
/// after every push, and [`Self::finish`] emits the stream trailer.
pub(crate) enum BodyStreamEncoder {
    None,
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
    Deflate(flate2::write::ZlibEncoder<Vec<u8>>),
    Brotli(Box<brotli::enc::writer::CompressorWriter<Vec<u8>>>),
}

fn new_brotli_vec_encoder() -> brotli::enc::writer::CompressorWriter<Vec<u8>> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 4,
        lgwin: 22,
        ..Default::default()
    };
    brotli::enc::writer::CompressorWriter::with_params(Vec::new(), STREAM_CHUNK_SIZE, &params)
}

impl BodyStreamEncoder {
    pub(crate) fn new(compression: Compression) -> Self {
        match compression {
            Compression::None => Self::None,
            Compression::Gzip => Self::Gzip(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::default(),
            )),
            Compression::Deflate => Self::Deflate(flate2::write::ZlibEncoder::new(
                Vec::new(),
                flate2::Compression::default(),
            )),
            Compression::Brotli => Self::Brotli(Box::new(new_brotli_vec_encoder())),
        }
    }

    pub(crate) fn encode_chunk(
        &mut self,
        chunk: Vec<u8>,
    ) -> Result<Vec<u8>, Report<TrustedServerError>> {
        match self {
            // Identity encoding passes the processed chunk through untouched.
            Self::None => Ok(chunk),
            Self::Gzip(encoder) => {
                encoder
                    .write_all(&chunk)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to encode gzip publisher body chunk".to_string(),
                    })?;
                Ok(std::mem::take(encoder.get_mut()))
            }
            Self::Deflate(encoder) => {
                encoder
                    .write_all(&chunk)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to encode deflate publisher body chunk".to_string(),
                    })?;
                Ok(std::mem::take(encoder.get_mut()))
            }
            Self::Brotli(encoder) => {
                encoder
                    .write_all(&chunk)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to encode brotli publisher body chunk".to_string(),
                    })?;
                Ok(std::mem::take(encoder.get_mut()))
            }
        }
    }

    /// Emits the encoder trailer. Consumes the codec state (the encoder
    /// becomes identity afterwards); terminal — call once at end of stream.
    pub(crate) fn finish(&mut self) -> Result<Vec<u8>, Report<TrustedServerError>> {
        match std::mem::replace(self, Self::None) {
            Self::None => Ok(Vec::new()),
            Self::Gzip(encoder) => encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize gzip publisher body encoder".to_string(),
            }),
            Self::Deflate(encoder) => encoder.finish().change_context(TrustedServerError::Proxy {
                message: "Failed to finalize deflate publisher body encoder".to_string(),
            }),
            Self::Brotli(encoder) => Ok((*encoder).into_inner()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming_replacer::{Replacement, StreamingReplacer};

    #[test]
    fn body_stream_decoder_enforces_cumulative_decoded_cap() {
        let compressed = {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(&vec![b'a'; 64 * 1024])
                .expect("should write gzip test input");
            encoder.finish().expect("should finish gzip encoding")
        };
        assert!(
            compressed.len() < 1024,
            "test precondition: compressed input must stay small"
        );
        let mut decoder = BodyStreamDecoder::new(Compression::Gzip, 1024);

        let err = decoder
            .decode_chunk(bytes::Bytes::from(compressed))
            .expect_err("decoded expansion past the cap must fail");

        assert!(
            format!("{err:?}").contains("decoded size exceeded"),
            "should report the cumulative decoded cap: {err:?}"
        );
    }

    #[test]
    fn body_stream_decoder_rejects_truncated_deflate_stream() {
        let compressed = {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(b"deflate payload that spans more than one deflate block boundary")
                .expect("should write deflate test input");
            encoder.finish().expect("should finish deflate encoding")
        };
        let truncated = &compressed[..compressed.len() / 2];
        let mut decoder = BodyStreamDecoder::new(Compression::Deflate, usize::MAX);
        decoder
            .decode_chunk(bytes::Bytes::copy_from_slice(truncated))
            .expect("partial deflate input should decode incrementally");

        let err = decoder
            .finish()
            .expect_err("truncated deflate stream must fail at finalization");

        assert!(
            format!("{err:?}").contains("truncated stream"),
            "should report the missing deflate end marker: {err:?}"
        );
    }

    #[test]
    fn body_stream_decoder_ignores_deflate_trailing_bytes() {
        let compressed = {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(b"deflate payload")
                .expect("should write deflate test input");
            encoder.finish().expect("should finish deflate encoding")
        };
        let mut with_trailing = compressed;
        with_trailing.extend_from_slice(b"junk");
        let mut decoder = BodyStreamDecoder::new(Compression::Deflate, usize::MAX);

        let decoded = decoder
            .decode_chunk(bytes::Bytes::from(with_trailing))
            .expect("complete deflate stream should decode");
        decoder
            .finish()
            .expect("trailing bytes after the end marker should be ignored");

        assert_eq!(
            decoded.as_ref(),
            b"deflate payload",
            "should decode the payload and drop trailing junk"
        );
    }

    #[test]
    fn body_stream_decoder_decodes_deflate_filling_output_buffer_exactly() {
        // A decoded length one byte past the decoder's internal output buffer
        // (`STREAM_CHUNK_SIZE`) hits the boundary where flate2 consumes all
        // input while exactly filling the output buffer and returns
        // `Status::Ok` with the stream-end marker still pending. The decoder
        // must drive the inflater to completion instead of reporting a
        // truncated stream.
        let payload = vec![b'a'; STREAM_CHUNK_SIZE + 1];
        let compressed = {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(&payload)
                .expect("should write deflate test input");
            encoder.finish().expect("should finish deflate encoding")
        };
        let mut decoder = BodyStreamDecoder::new(Compression::Deflate, usize::MAX);

        let mut decoded = decoder
            .decode_chunk(bytes::Bytes::from(compressed))
            .expect("complete deflate stream should decode")
            .to_vec();
        decoded.extend(
            decoder
                .finish()
                .expect("a complete deflate stream must not report truncation"),
        );

        assert_eq!(
            decoded, payload,
            "should decode the full payload across the output-buffer boundary"
        );
    }

    #[test]
    fn body_stream_decoder_decodes_deflate_split_across_many_chunks() {
        let payload = vec![b'x'; STREAM_CHUNK_SIZE * 3 + 7];
        let compressed = {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(&payload)
                .expect("should write deflate test input");
            encoder.finish().expect("should finish deflate encoding")
        };
        let mut decoder = BodyStreamDecoder::new(Compression::Deflate, usize::MAX);

        let mut decoded = Vec::new();
        // Feed the compressed stream a few bytes at a time to exercise many
        // input split points, including splits inside the end-of-stream marker.
        for piece in compressed.chunks(3) {
            decoded.extend(
                decoder
                    .decode_chunk(bytes::Bytes::copy_from_slice(piece))
                    .expect("partial deflate input should decode incrementally"),
            );
        }
        decoded.extend(
            decoder
                .finish()
                .expect("a complete deflate stream must finalize"),
        );

        assert_eq!(
            decoded, payload,
            "should decode the full payload regardless of input split points"
        );
    }

    fn gzip_member(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(data)
            .expect("should write gzip test input");
        encoder.finish().expect("should finish gzip encoding")
    }

    #[test]
    fn body_stream_decoder_decodes_multi_member_gzip_single_chunk() {
        let mut compressed = gzip_member(b"first member ");
        compressed.extend(gzip_member(b"second member"));
        let mut decoder = BodyStreamDecoder::new(Compression::Gzip, usize::MAX);

        let mut decoded = decoder
            .decode_chunk(bytes::Bytes::from(compressed))
            .expect("a multi-member gzip body must decode all members")
            .to_vec();
        decoded.extend(
            decoder
                .finish()
                .expect("a multi-member gzip body must finalize"),
        );

        assert_eq!(
            decoded, b"first member second member",
            "should concatenate the decoded output of every gzip member"
        );
    }

    #[test]
    fn body_stream_decoder_decodes_multi_member_gzip_split_across_chunks() {
        let mut compressed = gzip_member(b"alpha");
        compressed.extend(gzip_member(b"omega"));
        let mut decoder = BodyStreamDecoder::new(Compression::Gzip, usize::MAX);

        let mut decoded = Vec::new();
        for piece in compressed.chunks(4) {
            decoded.extend(
                decoder
                    .decode_chunk(bytes::Bytes::copy_from_slice(piece))
                    .expect("multi-member gzip should decode across chunk boundaries"),
            );
        }
        decoded.extend(
            decoder
                .finish()
                .expect("a multi-member gzip body must finalize"),
        );

        assert_eq!(
            decoded, b"alphaomega",
            "should decode both gzip members split across chunk boundaries"
        );
    }

    /// Verify that `lol_html` fragments text nodes when input chunks split
    /// mid-text-node. Script rewriters must be fragment-safe — they accumulate
    /// text fragments internally until `is_last_in_text_node` is true.
    #[test]
    fn lol_html_fragments_text_across_chunk_boundaries() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let fragments: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        let fragments_clone = Rc::clone(&fragments);

        let mut rewriter = lol_html::HtmlRewriter::new(
            lol_html::Settings {
                element_content_handlers: vec![lol_html::text!("script", move |text| {
                    fragments_clone
                        .borrow_mut()
                        .push((text.as_str().to_owned(), text.last_in_text_node()));
                    Ok(())
                })],
                ..lol_html::Settings::default()
            },
            |_chunk: &[u8]| {},
        );

        // Split "googletagmanager.com/gtm.js" across two chunks
        rewriter
            .write(b"<script>google")
            .expect("should write chunk1");
        rewriter
            .write(b"tagmanager.com/gtm.js</script>")
            .expect("should write chunk2");
        rewriter.end().expect("should end");

        let frags = fragments.borrow();
        // lol_html should emit at least 2 text fragments since input was split
        assert!(
            frags.len() >= 2,
            "should fragment text across chunk boundaries, got {} fragments: {:?}",
            frags.len(),
            *frags
        );
        // No single fragment should contain the full domain
        assert!(
            !frags
                .iter()
                .any(|(text, _)| text.contains("googletagmanager.com")),
            "no individual fragment should contain the full domain when split across chunks: {:?}",
            *frags
        );
    }

    #[test]
    fn test_uncompressed_pipeline() {
        let replacer = StreamingReplacer::new(vec![Replacement {
            find: "hello".to_owned(),
            replace_with: "hi".to_owned(),
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
        use lol_html::{Settings, element};

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
            large_html.push_str(&format!("<p>Paragraph {i}</p>"));
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
            find: "hello".to_owned(),
            replace_with: "hi".to_owned(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Deflate,
            output_compression: Compression::Deflate,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        pipeline
            .process(&*compressed_input, &mut output)
            .expect("should process deflate-to-deflate");

        // Decompress output and verify correctness
        let mut decompressed = Vec::new();
        ZlibDecoder::new(&*output)
            .read_to_end(&mut decompressed)
            .expect("should decompress output \u{2014} implies encoder was finalized correctly");

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
            find: "hello".to_owned(),
            replace_with: "hi".to_owned(),
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
            .process(&*compressed_input, &mut output)
            .expect("should process gzip-to-gzip");

        // Assert
        let mut decompressed = Vec::new();
        GzDecoder::new(&*output)
            .read_to_end(&mut decompressed)
            .expect("should decompress output \u{2014} implies encoder was finalized correctly");

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
            find: "hello".to_owned(),
            replace_with: "hi".to_owned(),
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
            .process(&*compressed_input, &mut output)
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
        use brotli::Decompressor;
        use brotli::enc::writer::CompressorWriter;
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
            find: "hello".to_owned(),
            replace_with: "hi".to_owned(),
        }]);

        let config = PipelineConfig {
            input_compression: Compression::Brotli,
            output_compression: Compression::Brotli,
            chunk_size: 8192,
        };

        let mut pipeline = StreamingPipeline::new(config, replacer);
        let mut output = Vec::new();

        pipeline
            .process(&*compressed_input, &mut output)
            .expect("should process brotli-to-brotli");

        // Decompress output and verify correctness
        let mut decompressed = Vec::new();
        Decompressor::new(&*output, 4096)
            .read_to_end(&mut decompressed)
            .expect("should decompress output \u{2014} implies encoder was finalized correctly");

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
        use lol_html::{Settings, element};

        let settings = Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href")
                    && href.contains("example.com")
                {
                    el.set_attribute("href", &href.replace("example.com", "test.com"))?;
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
        use lol_html::{Settings, element};
        use std::io::{Read as _, Write as _};

        let settings = Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href")
                    && href.contains("example.com")
                {
                    el.set_attribute("href", &href.replace("example.com", "test.com"))?;
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
            .process(&*compressed_input, &mut output)
            .expect("pipeline should process gzip HTML");

        let mut decompressed = Vec::new();
        GzDecoder::new(&*output)
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
