use super::*;
use crate::middleware::{PhaseEntries, PhaseEntry};
use crate::settings::ProxyAssetRoute;
use crate::test_support::tests::create_test_settings;
use edgezero_core::body::Body as EdgeBody;
use std::time::Duration;

const ROUTE: &str = "/_rehost/cdn.example.net/npm/";

fn settings_rehosting(stylesheets_under: &str) -> Settings {
    let mut settings = create_test_settings();
    let mut route = ProxyAssetRoute::new(ROUTE, "https://cdn.example.net/npm");
    route.rehost_page_references = true;
    settings.proxy.asset_routes.push(route);
    settings.serve = PhaseEntries::new(vec![PhaseEntry {
        media_type: "text/css".to_owned(),
        path: Some(format!("{ROUTE}{stylesheets_under}")),
        middleware: vec!["provenance.encypher.stylesheet".to_owned()],
    }]);
    settings
}

fn relocated(settings: &Settings, css: &str) -> String {
    let routes = RehostRoutes::of(settings);
    let stylesheet =
        Url::parse("https://cdn.example.net/npm/slick@1/slick/theme.css?ver=1").expect("url");
    rewrite_css_urls(
        &mut RouteAddresses {
            stylesheet: &stylesheet,
            routes: &routes,
            page: PageAddress {
                scheme: "https",
                host: "reader.example.com",
                removed_prefix: "/www.publisher.example",
                base: None,
            },
        },
        css,
        true,
        MAX_STYLESHEET_BYTES,
    )
    .expect("should rewrite")
}

#[test]
fn a_moved_stylesheet_s_addresses_keep_naming_what_they_named() {
    let css = concat!(
        "@import \"../shared/base.css\" layer(theme);\n",
        ".a{background:url(./ajax-loader.gif)}\n",
        "@font-face{src:url(\"fonts/slick.woff\") format(\"woff\"),url('fonts/slick.svg#slick')}\n",
        ".b{background:url(/root/x.png)}\n",
        ".c{background:image-set(\"https://elsewhere.example/a.png\" 1x)}\n",
        ".d{filter:url(#blur);background:url(data:image/png;base64,AAAA)}\n",
        ".e{background:url(\"fonts\\2f esc.png\")}\n",
    );
    let out = relocated(&settings_rehosting("slick@1/shared/"), css);
    let route = format!("https://reader.example.com/www.publisher.example{ROUTE}");
    for expected in [
        // An approved stylesheet path moves with the route.
        format!("@import \"{route}slick@1/shared/base.css\" layer(theme);"),
        format!("url(\"{route}slick@1/slick/ajax-loader.gif\")"),
        format!("url(\"{route}slick@1/slick/fonts/slick.woff\")"),
        format!("url(\"{route}slick@1/slick/fonts/slick.svg#slick\")"),
        format!("url(\"{route}slick@1/slick/fonts/esc.png\")"),
        // Outside the folder, so the absolute upstream address.
        "url(\"https://cdn.example.net/root/x.png\")".to_owned(),
        "\"https://elsewhere.example/a.png\" 1x".to_owned(),
        // Document-local and inline references stay as written.
        "url(#blur)".to_owned(),
        "url(data:image/png;base64,AAAA)".to_owned(),
    ] {
        assert!(out.contains(&expected), "should contain {expected}: {out}");
    }
}

#[test]
fn an_import_no_entry_moves_keeps_its_upstream_address() {
    let out = relocated(
        &settings_rehosting("other/"),
        "@import url(\"../shared/base.css\");",
    );
    assert!(
        out.contains("url(\"https://cdn.example.net/npm/slick@1/shared/base.css\")"),
        "a stylesheet no entry moves is fetched where it lives: {out}"
    );
}

#[test]
fn a_stylesheet_naming_another_charset_is_not_moved() {
    assert!(names_another_charset("@charset \"iso-8859-1\";\n.a{}"));
    assert!(!names_another_charset("@charset \"UTF-8\";\n.a{}"));
    assert!(!names_another_charset(".a{}"));
}

#[test]
fn relocated_cache_lifetimes_are_bounded_by_both_origin_and_route() {
    let route = CachePolicy {
        browser_ttl: Some(Duration::from_secs(60)),
        edge_ttl: Some(Duration::from_secs(600)),
        ..CachePolicy::public_immutable(Duration::from_secs(60))
    };
    for (browser, shared, expected_browser, expected_shared) in [
        (300, 3600, 60, 600),
        (20, 300, 20, 300),
        (20, 3600, 20, 600),
        (300, 20, 60, 20),
        (0, 300, 0, 300),
        (20, 0, 20, 0),
    ] {
        let policy = bounded_cache_policy(
            OriginFreshness {
                browser: Duration::from_secs(browser),
                shared: Duration::from_secs(shared),
            },
            Some(route),
        );
        assert_eq!(
            policy.browser_ttl,
            Some(Duration::from_secs(expected_browser))
        );
        assert_eq!(policy.edge_ttl, Some(Duration::from_secs(expected_shared)));
        assert!(!policy.immutable);
        assert_eq!(policy.stale_while_revalidate, None);
        assert_eq!(policy.stale_if_error, None);
    }
    let policy = bounded_cache_policy(
        OriginFreshness {
            browser: Duration::from_secs(20),
            shared: Duration::from_secs(300),
        },
        None,
    );
    assert_eq!(policy.browser_ttl, Some(Duration::from_secs(20)));
    assert_eq!(policy.edge_ttl, Some(Duration::from_secs(300)));
}

#[test]
fn buffered_stylesheets_keep_their_allocation_up_to_the_bound() {
    futures::executor::block_on(async {
        for length in [0, 16, MAX_STYLESHEET_BYTES] {
            let bytes = Bytes::from(vec![b' '; length]);
            let pointer = bytes.as_ptr();
            let collected = collect_stylesheet(EdgeBody::Once(bytes))
                .await
                .expect("bounded buffered stylesheet");
            assert_eq!(collected.len(), length);
            assert_eq!(collected.as_ptr(), pointer, "buffered bytes stay zero-copy");
        }
        assert!(
            collect_stylesheet(EdgeBody::Once(Bytes::from(vec![
                0;
                MAX_STYLESHEET_BYTES + 1
            ])))
            .await
            .is_err(),
            "oversized buffered bytes are refused"
        );
    });
}

#[test]
fn streamed_stylesheets_refuse_oversized_chunks_without_reading_on() {
    futures::executor::block_on(async {
        for first_length in [0, MAX_STYLESHEET_BYTES - 1] {
            let body = EdgeBody::from_stream(async_stream::stream! {
                if first_length != 0 {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(vec![0; first_length]));
                }
                // Includes both a single oversized chunk and a chunk larger
                // than the remaining capacity after a valid prefix.
                let length = if first_length == 0 { MAX_STYLESHEET_BYTES + 1 } else { 2 };
                yield Ok(Bytes::from(vec![0; length]));
                panic!("must refuse the oversized chunk before reading another");
            });
            let error = collect_stylesheet(body)
                .await
                .expect_err("must refuse excess");
            assert!(matches!(
                error.current_context(),
                TrustedServerError::Integration { .. }
            ));
        }
    });
}

#[test]
fn streamed_stylesheets_accept_the_exact_bound_and_report_upstream_errors() {
    futures::executor::block_on(async {
        let body = EdgeBody::stream(futures::stream::iter([
            Bytes::from(vec![b'a'; MAX_STYLESHEET_BYTES - 1]),
            Bytes::from_static(b"b"),
        ]));
        let bytes = collect_stylesheet(body)
            .await
            .expect("exactly bounded stream");
        assert_eq!(bytes.len(), MAX_STYLESHEET_BYTES);
        assert_eq!(bytes.last(), Some(&b'b'));

        let body = EdgeBody::from_stream(futures::stream::iter([
            Ok(Bytes::from_static(b"partial")),
            Err(std::io::Error::other("origin reset")),
        ]));
        let error = collect_stylesheet(body)
            .await
            .expect_err("upstream failure");
        assert!(matches!(
            error.current_context(),
            TrustedServerError::Integration { .. }
        ));
    });
}
