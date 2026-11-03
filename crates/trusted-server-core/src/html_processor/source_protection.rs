//! Bounded original-source hold around the entire HTML transformation pass.
//! Classification belongs to the selected middleware, not to core.

use std::io;
use std::ops::Range;

use futures::future::LocalBoxFuture;

use crate::middleware::{MiddlewareChain, SourceProtection};
use crate::platform::RuntimeServices;
use crate::streaming_processor::StreamProcessor;

pub(super) struct SourceProtected<P> {
    inner: P,
    classifiers: Vec<SourceProtection>,
    original: Vec<u8>,
    limit: usize,
    ended: bool,
}

impl<P> SourceProtected<P> {
    pub(super) fn new(inner: P, chains: &[&MiddlewareChain], limit: usize) -> Self {
        Self {
            inner,
            classifiers: chains
                .iter()
                .flat_map(|chain| chain.source_protections())
                .collect(),
            original: Vec::new(),
            limit,
            ended: false,
        }
    }

    fn hold(&mut self, chunk: &[u8]) -> io::Result<()> {
        if self.ended {
            return Err(refuse("source protection processor already ended"));
        }
        append(&mut self.original, chunk, self.limit)
    }

    fn mask(&mut self) -> io::Result<MaskedDocument> {
        self.ended = true;
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for classify in &self.classifiers {
            let classified = classify(&self.original)?;
            let mut previous_end = 0;
            for range in &classified {
                if range.start < previous_end
                    || range.start >= range.end
                    || range.end > self.original.len()
                {
                    return Err(refuse("source protection returned invalid ranges"));
                }
                previous_end = range.end;
            }
            ranges.extend(classified);
        }
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(last) = merged.last_mut().filter(|last| range.start <= last.end) {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        MaskedDocument::new(std::mem::take(&mut self.original), merged, self.limit)
    }
}

impl<P: StreamProcessor> StreamProcessor for SourceProtected<P> {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> io::Result<Vec<u8>> {
        if self.classifiers.is_empty() {
            return self.inner.process_chunk(chunk, is_last);
        }
        self.hold(chunk)?;
        if !is_last {
            return Ok(Vec::new());
        }
        let masked = self.mask()?;
        let input = masked.input(self.limit)?;
        let output = self.inner.process_chunk(&input, true)?;
        masked.restore(output, self.limit)
    }

    fn finish<'a>(
        &'a mut self,
        chunk: &'a [u8],
        services: &'a RuntimeServices,
    ) -> LocalBoxFuture<'a, io::Result<Vec<u8>>> {
        Box::pin(async move {
            if self.classifiers.is_empty() {
                return self.inner.finish(chunk, services).await;
            }
            self.hold(chunk)?;
            let masked = self.mask()?;
            let input = masked.input(self.limit)?;
            let output = self.inner.finish(&input, services).await?;
            masked.restore(output, self.limit)
        })
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.original.clear();
        self.ended = false;
    }
}

struct MaskedDocument {
    // Retain one original allocation. Protected SVGs remain slices, never copies.
    original: Vec<u8>,
    ranges: Vec<Range<usize>>,
    prefix: String,
}

impl MaskedDocument {
    fn new(original: Vec<u8>, ranges: Vec<Range<usize>>, limit: usize) -> io::Result<Self> {
        if original.len() > limit {
            return Err(refuse("source protection original exceeds body limit"));
        }
        let prefix = if ranges.is_empty() {
            String::new()
        } else {
            format!("<!--ts-source-{}-", uuid::Uuid::new_v4().simple())
        };
        if !prefix.is_empty() && find(&original, prefix.as_bytes()).is_some() {
            return Err(refuse("source protection marker collides with original"));
        }
        Ok(Self {
            original,
            ranges,
            prefix,
        })
    }

    fn marker(&self, index: usize) -> String {
        format!("{}{index}-->", self.prefix)
    }

    fn input(&self, limit: usize) -> io::Result<std::borrow::Cow<'_, [u8]>> {
        if self.ranges.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(&self.original));
        }
        let mut input = Vec::new();
        let mut cursor = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            append(&mut input, &self.original[cursor..range.start], limit)?;
            append(&mut input, self.marker(index).as_bytes(), limit)?;
            cursor = range.end;
        }
        append(&mut input, &self.original[cursor..], limit)?;
        Ok(std::borrow::Cow::Owned(input))
    }

    fn restore(&self, output: Vec<u8>, limit: usize) -> io::Result<Vec<u8>> {
        if output.len() > limit {
            return Err(refuse("source protection output exceeds body limit"));
        }
        if self.ranges.is_empty() {
            return Ok(output);
        }
        let mut restored = Vec::new();
        let mut cursor = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            let at = find(&output[cursor..], self.prefix.as_bytes())
                .map(|at| cursor + at)
                .ok_or_else(|| refuse("source protection marker lost"))?;
            let marker = self.marker(index);
            if !output[at..].starts_with(marker.as_bytes()) {
                return Err(refuse(
                    "source protection marker duplicated, reordered or altered",
                ));
            }
            append(&mut restored, &output[cursor..at], limit)?;
            append(&mut restored, &self.original[range.clone()], limit)?;
            cursor = at + marker.len();
        }
        if find(&output[cursor..], self.prefix.as_bytes()).is_some() {
            return Err(refuse("source protection marker duplicated"));
        }
        append(&mut restored, &output[cursor..], limit)?;
        Ok(restored)
    }
}

fn append(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> io::Result<()> {
    if bytes.len() > limit.saturating_sub(output.len()) {
        return Err(refuse("source protection exceeds body limit"));
    }
    // Reserve explicitly so Vec's implicit growth cannot overshoot the bound.
    let needed = output.len() + bytes.len();
    if needed > output.capacity() {
        let capacity = output.capacity().saturating_mul(2).max(needed).min(limit);
        output.reserve_exact(capacity - output.len());
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn refuse(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::{
        HTML_MEDIA_TYPE, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
    };
    use crate::platform::test_support::noop_services;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Policy(SourceProtection);

    impl Middleware for Policy {
        fn middleware_id(&self) -> &'static str {
            "test.protection"
        }
        fn phases(&self) -> &[MiddlewarePhase] {
            &MiddlewarePhase::ALL
        }
        fn source_protection(&self) -> Option<SourceProtection> {
            Some(self.0)
        }
        fn create(&self, _: &MiddlewareContext<'_>) -> MiddlewareAction {
            MiddlewareAction::pass()
        }
    }

    struct Replace {
        calls: Arc<AtomicUsize>,
        transform: fn(&[u8]) -> Vec<u8>,
    }

    impl StreamProcessor for Replace {
        fn process_chunk(&mut self, chunk: &[u8], _: bool) -> io::Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok((self.transform)(chunk))
        }
    }

    fn processor(
        classify: SourceProtection,
        transform: fn(&[u8]) -> Vec<u8>,
        limit: usize,
    ) -> (SourceProtected<Replace>, Arc<AtomicUsize>) {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Serve,
            HTML_MEDIA_TYPE,
            vec![Arc::new(Policy(classify))],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        (
            SourceProtected::new(
                Replace {
                    calls: Arc::clone(&calls),
                    transform,
                },
                &[&chain],
                limit,
            ),
            calls,
        )
    }

    fn middle(_: &[u8]) -> io::Result<Vec<Range<usize>>> {
        Ok(std::iter::once(1..5).collect())
    }
    fn unchanged(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }

    #[test]
    fn holds_original_until_complete_and_restores_exact_bytes_after_async_finish() {
        let (mut processor, calls) = processor(
            middle,
            |bytes| {
                let mut output = b"outside".to_vec();
                output.extend_from_slice(&bytes[1..]);
                output
            },
            1024,
        );
        assert!(
            processor
                .process_chunk(b"aKEEP", false)
                .expect("hold")
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0, "no early transform");
        let result =
            futures::executor::block_on(processor.finish(b"z", &noop_services())).expect("restore");
        assert_eq!(result, b"outsideKEEPz");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn source_limit_is_checked_before_copying_or_running_transforms() {
        let (mut processor, calls) = processor(middle, unchanged, 5);
        assert!(
            processor
                .process_chunk(b"a", false)
                .expect("hold")
                .is_empty()
        );
        let before = processor.original.capacity();
        assert!(processor.process_chunk(&[b'x'; 128], false).is_err());
        assert_eq!(processor.original, b"a");
        assert_eq!(processor.original.capacity(), before);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn refuses_invalid_ranges_without_exposing_output() {
        for classify in [
            (|_: &[u8]| Ok(std::iter::once(4..7).collect())) as SourceProtection,
            |_: &[u8]| Ok(std::iter::once(2..2).collect()),
            |_: &[u8]| Ok(vec![1..4, 3..5]),
            |_: &[u8]| Ok(vec![3..5, 0..1]),
            |_: &[u8]| Err(refuse("classification failed")),
        ] {
            let (mut processor, calls) = processor(classify, unchanged, 1024);
            assert!(processor.process_chunk(b"aKEEPz", true).is_err());
            assert_eq!(calls.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn refuses_lost_duplicated_reordered_and_altered_markers() {
        let masked =
            MaskedDocument::new(b"aKEEPzOTHERq".to_vec(), vec![1..5, 6..11], 1024).expect("mask");
        let first = masked.marker(0);
        let second = masked.marker(1);
        for invalid in [
            format!("a{second}q"),
            format!("a{first}{first}{second}q"),
            format!("a{second}{first}q"),
            format!("a{}x-->{second}q", masked.prefix),
            format!("a{first}{second}{second}q"),
        ] {
            assert!(masked.restore(invalid.into_bytes(), 1024).is_err());
        }
        assert_eq!(
            masked
                .restore(format!("edited{first}z{second}q").into_bytes(), 1024)
                .expect("restore"),
            b"editedKEEPzOTHERq"
        );
    }

    #[test]
    fn refuses_mask_and_restoration_expansion_past_limit() {
        let short = MaskedDocument::new(b"aKEEPz".to_vec(), std::iter::once(1..5).collect(), 6)
            .expect("original fits");
        assert!(short.input(6).is_err(), "opaque marker must fit too");
        let original = format!("a{}z", "K".repeat(128)).into_bytes();
        let limit = original.len();
        let masked = MaskedDocument::new(
            original.clone(),
            std::iter::once(1..limit - 1).collect(),
            limit,
        )
        .expect("fits");
        let input = masked.input(limit).expect("mask fits").into_owned();
        assert!(masked.restore(input.clone(), limit - 1).is_err());
        assert_eq!(masked.restore(input, limit).expect("exact limit"), original);
    }

    #[test]
    fn policy_is_default_off_and_selected_only_from_supplied_chains() {
        let chain = MiddlewareChain::new(MiddlewarePhase::Serve, HTML_MEDIA_TYPE, Vec::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut processor = SourceProtected::new(
            Replace {
                calls: Arc::clone(&calls),
                transform: unchanged,
            },
            &[&chain],
            1,
        );
        assert_eq!(
            processor.process_chunk(b"unheld", false).expect("stream"),
            b"unheld"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
