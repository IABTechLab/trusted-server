//! Spin startup diagnostics.

use std::io;

/// Emits a startup diagnostic without claiming the process-global logger.
///
/// Spin captures component stderr as its supported runtime log. Writing this
/// one boot failure directly keeps the diagnostic observable even while the
/// `EdgeZero` Spin logger initializer is a no-op, without filtering application
/// logs or preventing a future backend from installing the global logger.
pub(crate) fn emit_startup_diagnostic(message: &str) {
    let _ = write_startup_diagnostic(&mut io::stderr().lock(), message);
}

fn write_startup_diagnostic(writer: &mut impl io::Write, message: &str) -> io::Result<()> {
    writeln!(writer, "{message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_fallback_writes_one_complete_diagnostic() {
        let mut output = Vec::new();

        write_startup_diagnostic(&mut output, "Spin startup failed")
            .expect("should write the startup diagnostic");

        assert_eq!(output, b"Spin startup failed\n");
    }
}
