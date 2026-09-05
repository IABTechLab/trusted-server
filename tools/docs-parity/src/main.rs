use std::io::{self, Write as _};
use std::process;

fn main() {
    match docs_parity::run_from_env() {
        Ok(outcome) if outcome.exit_code() != docs_parity::EXIT_SUCCESS => {
            process::exit(outcome.exit_code());
        }
        Ok(_) => {}
        Err(report) => {
            let mut standard_error = io::stderr().lock();
            let _ = writeln!(standard_error, "docs-parity: {report:?}");
            process::exit(docs_parity::EXIT_ERROR);
        }
    }
}
