//! What this build reports about itself, fixed when it was compiled.
//!
//! `env!` is resolved by the compiler. Some hosts give a WebAssembly guest no
//! custom environment variable when it runs, so a fact read then would read
//! as absent there.
//!
//! | Constant | Build input | Without it |
//! | --- | --- | --- |
//! | [`BUILT_AT`] | `SOURCE_DATE_EPOCH`, in seconds | The time of the build |
//! | [`COMMIT`] | `TRUSTED_SERVER_COMMIT` | `unknown` |
//! | [`BUILD_RUN`] | `TRUSTED_SERVER_BUILD_RUN` | `unknown` |
//! | [`BUILD_TAG`] | `TRUSTED_SERVER_BUILD_TAG` | `unknown` |
//!
//! Each is the builder's own report. Nothing here measures the binary.

/// The crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// When this build was made, RFC 3339 in UTC to the second.
pub const BUILT_AT: &str = env!("TRUSTED_SERVER_BUILT_AT");

/// The commit this build was made from, or `unknown`.
pub const COMMIT: &str = env!("TRUSTED_SERVER_COMMIT");

/// The build run that made this binary, or `unknown`.
pub const BUILD_RUN: &str = env!("TRUSTED_SERVER_BUILD_RUN");

/// The tag this build was made from, as the builder names it, or `unknown`.
pub const BUILD_TAG: &str = env!("TRUSTED_SERVER_BUILD_TAG");

/// The build script's date arithmetic, compiled here so a test can call it.
#[cfg(test)]
#[path = "../build_time.rs"]
mod build_time;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_time_is_rfc_3339_in_utc() {
        let shape = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
            .expect("should compile the pattern");

        assert!(
            shape.is_match(BUILT_AT),
            "should be RFC 3339 in UTC: {BUILT_AT}"
        );
    }

    #[test]
    fn the_commit_the_run_and_the_tag_are_never_empty() {
        for (name, value) in [
            ("commit", COMMIT),
            ("build run", BUILD_RUN),
            ("build tag", BUILD_TAG),
        ] {
            assert!(
                !value.is_empty() && !value.chars().any(char::is_control),
                "the {name} should be the builder's value or `unknown`, got {value:?}"
            );
        }
    }

    #[test]
    fn the_build_script_writes_known_instants_as_rfc_3339() {
        for (seconds, expected, instant) in [
            (0, "1970-01-01T00:00:00Z", "the epoch"),
            (
                951_782_400,
                "2000-02-29T00:00:00Z",
                "the leap day of a 400th year",
            ),
            (1_000_000_000, "2001-09-09T01:46:40Z", "a time of day"),
            (4_102_444_800, "2100-01-01T00:00:00Z", "the start of 2100"),
            (
                4_107_542_400,
                "2100-03-01T00:00:00Z",
                "the day after 28 February 2100",
            ),
        ] {
            assert_eq!(
                build_time::rfc3339_utc(seconds),
                expected,
                "should write {instant}, {seconds} seconds after the epoch"
            );
        }
    }
}
