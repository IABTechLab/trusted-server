//! Asking the hosting platform to purge a surrogate key.

use trusted_server_core::platform::{
    TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY, reader_url_surrogate_key,
};

use crate::commands::cache::PurgeArgs;
use crate::error::{CliResult, cli_error};
use crate::fastly_cli::{FastlyRunner, args};

/// What one purge names: the scope it is reported under and the surrogate key purged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PurgeTarget {
    scope: &'static str,
    surrogate_key: String,
}

/// Works out what `purge` asks for.
///
/// # Errors
///
/// Returns a message when neither scope is given, when `--page` is not an absolute
/// http(s) URL, or when the service id could be read as anything but an id.
pub(crate) fn purge_target(purge: &PurgeArgs) -> CliResult<PurgeTarget> {
    // A Fastly service id holds letters and digits only.
    if purge.service_id.is_empty() || !purge.service_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return cli_error("--service-id must be a Fastly service id, of letters and digits only");
    }
    match (purge.all, purge.page.as_deref()) {
        (true, _) => Ok(PurgeTarget {
            scope: "all",
            surrogate_key: TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY.to_owned(),
        }),
        // A page that is not an absolute URL names a key nothing was stored with.
        (false, Some(page)) => {
            if !url::Url::parse(page).is_ok_and(|parsed| {
                matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
            }) {
                return cli_error("--page must be an absolute http(s) URL with a host");
            }
            Ok(PurgeTarget {
                scope: "url",
                surrogate_key: reader_url_surrogate_key(page),
            })
        }
        // A bare `ts cache purge` purges nothing.
        (false, None) => cli_error("specify --all or --page <url>"),
    }
}

/// Execute `ts cache purge` through `runner`.
///
/// # Errors
///
/// Returns an error when `purge` names nothing to purge, when the platform refuses the
/// purge, or when the result cannot be written.
pub(crate) fn run_purge(
    purge: &PurgeArgs,
    runner: &dyn FastlyRunner,
    out: &mut impl std::io::Write,
) -> CliResult<()> {
    let target = purge_target(purge)?;

    // By surrogate key for both scopes, never `fastly service purge --all`.
    runner
        .run(
            &args(&[
                "service",
                "purge",
                &format!("--key={}", target.surrogate_key),
                &format!("--service-id={}", purge.service_id),
                "--non-interactive",
            ]),
            None,
        )
        .map_err(|message| format!("purge failed: {message}"))?;

    // A success acknowledges invalidation of the key, not that an object existed.
    let report = serde_json::json!({
        "purged": true,
        "scope": target.scope,
        "surrogate_key": target.surrogate_key,
        "service_id": purge.service_id,
    });
    writeln!(out, "{report}").map_err(|error| format!("failed to write the purge result: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const SERVICE: &str = "ExampleServiceId0123456";

    /// Plays the `fastly` CLI, recording every invocation and failing when told to.
    #[derive(Debug, Default)]
    struct FakeFastly {
        calls: Mutex<Vec<Vec<String>>>,
        failure: Option<&'static str>,
    }

    impl FakeFastly {
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("should lock").clone()
        }
    }

    impl FastlyRunner for FakeFastly {
        fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
            assert!(stdin.is_none(), "a purge sends nothing on standard input");
            self.calls.lock().expect("should lock").push(args.to_vec());
            self.failure
                .map_or_else(|| Ok(String::new()), |message| Err(message.to_owned()))
        }
    }

    fn purge_args(all: bool, page: Option<&str>) -> PurgeArgs {
        PurgeArgs {
            service_id: SERVICE.to_owned(),
            all,
            page: page.map(str::to_owned),
        }
    }

    /// The value joined to `name` with `=`.
    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.iter()
            .find_map(|arg| arg.strip_prefix(name)?.strip_prefix('='))
    }

    fn purge(fake: &FakeFastly, purge: &PurgeArgs) -> CliResult<serde_json::Value> {
        let mut out = Vec::new();
        run_purge(purge, fake, &mut out)?;
        Ok(serde_json::from_slice(&out).expect("should report JSON"))
    }

    #[test]
    fn purge_all_names_the_key_every_cached_object_is_tagged_with() {
        let fake = FakeFastly::default();

        let report = purge(&fake, &purge_args(true, None)).expect("should purge");

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "one purge is one call: {calls:?}");
        assert_eq!(calls[0][..2], ["service", "purge"]);
        assert_eq!(flag(&calls[0], "--key"), Some("ts-template"));
        assert_eq!(flag(&calls[0], "--service-id"), Some(SERVICE));
        assert!(
            !calls[0].iter().any(|arg| arg == "--all"),
            "a purge of everything the service cached would reach untagged objects: {calls:?}"
        );
        assert!(
            calls[0].iter().any(|arg| arg == "--non-interactive"),
            "fastly must never wait on a prompt: {calls:?}"
        );
        assert_eq!(report["purged"], true);
        assert_eq!(report["scope"], "all");
        assert_eq!(report["surrogate_key"], "ts-template");
    }

    #[test]
    fn purge_page_names_the_key_the_service_tags_that_page_with() {
        let fake = FakeFastly::default();
        let page = "https://example.com/article?b=2&a=1";

        let report = purge(&fake, &purge_args(false, Some(page))).expect("should purge");

        let expected = reader_url_surrogate_key(page);
        assert_eq!(
            flag(&fake.calls()[0], "--key"),
            Some(expected.as_str()),
            "should purge the key the service derives for the page"
        );
        assert_eq!(report["scope"], "url");
        assert_eq!(report["surrogate_key"], expected.as_str());
    }

    #[test]
    fn two_spellings_of_one_page_purge_one_key() {
        let key_of = |page: &str| {
            purge_target(&purge_args(false, Some(page)))
                .expect("should name a key")
                .surrogate_key
        };

        assert_eq!(
            key_of("https://EXAMPLE.com:443/article/?b=2&a=1"),
            key_of("https://example.com/article?a=1&b=2"),
            "should canonicalize as the service does when it tags the page"
        );
        assert_ne!(
            key_of("http://example.com/article"),
            key_of("https://example.com/article"),
            "the scheme is part of the page a reader addresses"
        );
    }

    #[test]
    fn invalid_page_urls_are_rejected_before_fastly_is_run() {
        for page in [
            "/article",
            "example.com/article",
            "ftp://example.com/article",
            "",
        ] {
            let fake = FakeFastly::default();

            let error =
                purge(&fake, &purge_args(false, Some(page))).expect_err("should refuse the page");

            assert!(
                error.contains("--page"),
                "should name the flag for {page:?}: {error}"
            );
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
        }
    }

    #[test]
    fn every_value_is_joined_to_its_flag() {
        let fake = FakeFastly::default();

        purge(
            &fake,
            &purge_args(false, Some("https://example.com/article")),
        )
        .expect("should purge");

        for arg in &fake.calls()[0][2..] {
            assert!(
                arg.starts_with("--"),
                "a value standing alone can be read as a flag: {:?}",
                fake.calls()
            );
        }
    }

    #[test]
    fn neither_scope_is_an_error_rather_than_a_default() {
        let fake = FakeFastly::default();

        let error = purge(&fake, &purge_args(false, None)).expect_err("should refuse");

        assert!(error.contains("--all"), "should name the choices: {error}");
        assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
    }

    #[test]
    fn a_service_id_that_fastly_could_read_as_a_flag_is_refused() {
        for service_id in ["", "--all", "-s", "abc def", "abc/def"] {
            let fake = FakeFastly::default();
            let refused = PurgeArgs {
                service_id: service_id.to_owned(),
                all: true,
                page: None,
            };

            let error = purge(&fake, &refused).expect_err("should refuse the service id");

            assert!(error.contains("--service-id"), "{error}");
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
        }
    }

    #[test]
    fn a_refused_purge_reports_what_the_platform_said_and_prints_no_result() {
        let fake = FakeFastly {
            failure: Some("`fastly service purge` failed: 403 Forbidden"),
            ..FakeFastly::default()
        };
        let mut out = Vec::new();

        let error = run_purge(&purge_args(true, None), &fake, &mut out).expect_err("should fail");

        assert!(error.contains("403 Forbidden"), "{error}");
        assert!(out.is_empty(), "should not report a successful purge");
    }
}
