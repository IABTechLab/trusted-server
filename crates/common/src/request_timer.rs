//! Lightweight per-request timer for profiling the Fastly Compute request lifecycle.
//!
//! Records phase durations using [`std::time::Instant`] and emits them as a
//! [`Server-Timing`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Server-Timing)
//! header value so that timings are visible in browser `DevTools` (Network → Timing tab).

use std::time::Instant;

/// Records wall-clock durations for each phase of a request.
///
/// Usage:
/// ```ignore
/// let mut timer = RequestTimer::new();          // captures t0
/// // ... init work ...
/// timer.mark_init();                            // captures init duration
/// // ... backend fetch ...
/// timer.mark_backend();                         // captures backend duration
/// // ... body processing ...
/// timer.mark_process();                         // captures process duration
/// response.set_header("Server-Timing", timer.header_value());
/// ```
pub struct RequestTimer {
    start: Instant,
    init_ms: Option<f64>,
    backend_ms: Option<f64>,
    process_ms: Option<f64>,
    last_mark: Instant,
}

impl RequestTimer {
    /// Start a new timer. Call this as early as possible in `main()`.
    #[must_use]
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            init_ms: None,
            backend_ms: None,
            process_ms: None,
            last_mark: now,
        }
    }

    /// Mark the end of the initialization phase (settings, orchestrator, registry).
    pub fn mark_init(&mut self) {
        let now = Instant::now();
        self.init_ms = Some(duration_ms(self.last_mark, now));
        self.last_mark = now;
    }

    /// Mark the end of the backend fetch phase.
    pub fn mark_backend(&mut self) {
        let now = Instant::now();
        self.backend_ms = Some(duration_ms(self.last_mark, now));
        self.last_mark = now;
    }

    /// Mark the end of body processing (decompress, rewrite, recompress).
    pub fn mark_process(&mut self) {
        let now = Instant::now();
        self.process_ms = Some(duration_ms(self.last_mark, now));
        self.last_mark = now;
    }

    /// Total elapsed time since the timer was created.
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        duration_ms(self.start, Instant::now())
    }

    /// Format as a `Server-Timing` header value.
    ///
    /// Example output:
    /// `init;dur=1.2, backend;dur=385.4, process;dur=12.3, total;dur=401.5`
    #[must_use]
    pub fn header_value(&self) -> String {
        let mut parts = Vec::with_capacity(4);

        if let Some(ms) = self.init_ms {
            parts.push(format!("init;dur={ms:.1}"));
        }
        if let Some(ms) = self.backend_ms {
            parts.push(format!("backend;dur={ms:.1}"));
        }
        if let Some(ms) = self.process_ms {
            parts.push(format!("process;dur={ms:.1}"));
        }

        parts.push(format!("total;dur={:.1}", self.total_ms()));
        parts.join(", ")
    }

    /// Format a single-line log string for Fastly logs.
    #[must_use]
    pub fn log_line(&self) -> String {
        format!(
            "RequestTimer: init={:.1}ms backend={:.1}ms process={:.1}ms total={:.1}ms",
            self.init_ms.unwrap_or(0.0),
            self.backend_ms.unwrap_or(0.0),
            self.process_ms.unwrap_or(0.0),
            self.total_ms(),
        )
    }
}

impl Default for RequestTimer {
    fn default() -> Self {
        Self::new()
    }
}

fn duration_ms(from: Instant, to: Instant) -> f64 {
    to.duration_since(from).as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_value_includes_all_phases() {
        let mut timer = RequestTimer::new();
        timer.mark_init();
        timer.mark_backend();
        timer.mark_process();

        let header = timer.header_value();
        assert!(header.contains("init;dur="), "missing init phase");
        assert!(header.contains("backend;dur="), "missing backend phase");
        assert!(header.contains("process;dur="), "missing process phase");
        assert!(header.contains("total;dur="), "missing total phase");
    }

    #[test]
    fn header_value_omits_unmarked_phases() {
        let timer = RequestTimer::new();
        let header = timer.header_value();
        assert!(!header.contains("init;dur="));
        assert!(!header.contains("backend;dur="));
        assert!(header.contains("total;dur="));
    }

    #[test]
    fn log_line_uses_zero_for_unmarked() {
        let timer = RequestTimer::new();
        let log = timer.log_line();
        assert!(log.contains("init=0.0ms"));
        assert!(log.contains("backend=0.0ms"));
        assert!(log.contains("process=0.0ms"));
    }
}
