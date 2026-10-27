use super::*;
use crate::middleware::PhaseEntry;
use crate::source::OriginSettings;

const PREFIX: &str = "/_rehost/cdn.example.net/npm/";

fn rehosting(prefix: &str, url: &str) -> ProxyAssetRoute {
    let mut route = ProxyAssetRoute::new(prefix, url);
    route.rehost_page_references = true;
    route
}

fn stylesheets_moved_under(path: &str) -> PhaseEntries {
    PhaseEntries::new(vec![PhaseEntry {
        media_type: STYLESHEET_MEDIA_TYPE.to_owned(),
        path: Some(path.to_owned()),
        middleware: vec!["provenance.encypher.stylesheet".to_owned()],
    }])
}

fn routes(serve: PhaseEntries) -> RehostRoutes {
    RehostRoutes::of_parts(&[rehosting(PREFIX, "https://cdn.example.net/npm")], serve)
}

const PAGE: PageAddress<'static> = PageAddress {
    scheme: "https",
    host: "reader.example.com",
    removed_prefix: "/www.publisher.example",
    base: None,
};

#[test]
fn a_route_path_maps_under_the_source_folder_and_never_leaves_it() {
    assert_eq!(
        target_path(PREFIX, &format!("{PREFIX}lib@1/dist/a.css")).expect("should map"),
        "/lib@1/dist/a.css"
    );
    for refused in [
        PREFIX.to_owned(),
        format!("{PREFIX}lib/../x.css"),
        format!("{PREFIX}lib/%2e%2E/x.css"),
        format!("{PREFIX}lib/.%2e/x.css"),
        format!("{PREFIX}lib%2Fx.css"),
        format!("{PREFIX}lib%5cx.css"),
        "/other/x.css".to_owned(),
    ] {
        assert!(
            target_path(PREFIX, &refused).is_err(),
            "should refuse {refused}"
        );
    }
}

#[test]
fn a_media_address_on_the_folder_moves_with_its_query_and_fragment_as_written() {
    let decided = routes(PhaseEntries::default()).decide(
        "https://cdn.example.net/npm/slick@1/fonts/slick.woff?v=1&a=%2F#icon",
        Reference::Media,
        false,
        &PAGE,
    );
    assert_eq!(
        decided,
        Decision::Rehost(format!(
            "https://reader.example.com/www.publisher.example{PREFIX}slick@1/fonts/slick.woff?v=1&a=%2F#icon"
        ))
    );
}

#[test]
fn a_protocol_relative_address_takes_the_page_scheme_and_is_never_upgraded() {
    let routes = routes(PhaseEntries::default());
    assert!(matches!(
        routes.decide(
            "//cdn.example.net/npm/a.png",
            Reference::Media,
            false,
            &PAGE
        ),
        Decision::Rehost(_)
    ));
    let plain_page = PageAddress {
        scheme: "http",
        ..PAGE
    };
    assert_eq!(
        routes.decide(
            "//cdn.example.net/npm/a.png",
            Reference::Media,
            false,
            &plain_page
        ),
        Decision::Unmatched,
        "an http page fetches the http address, which no https route serves"
    );
    assert_eq!(
        routes.decide(
            "http://cdn.example.net/npm/a.png",
            Reference::Media,
            false,
            &PAGE
        ),
        Decision::Unmatched
    );
}

#[test]
fn only_the_folder_is_served() {
    let routes = routes(PhaseEntries::default());
    for other in [
        "https://cdn.example.net/npmx/a.png",
        "https://cdn.example.net/a.png",
        "https://other.example.net/npm/a.png",
        "https://cdn.example.net:8443/npm/a.png",
        "/npm/a.png",
        "npm/a.png",
        "data:image/png;base64,AAAA",
        "https://user@cdn.example.net/npm/a.png",
    ] {
        assert_eq!(
            routes.decide(other, Reference::Media, false, &PAGE),
            Decision::Unmatched,
            "{other}"
        );
    }
}

#[test]
fn a_protected_or_unapproved_address_on_the_folder_is_kept_as_written() {
    let routes = routes(stylesheets_moved_under(&format!("{PREFIX}moved/")));
    assert_eq!(
        routes.decide(
            "https://cdn.example.net/npm/a.png?return=https://publisher.example/x",
            Reference::Media,
            true,
            &PAGE
        ),
        Decision::Keep,
        "integrity is never broken, and nothing else may rewrite it"
    );
    assert_eq!(
        routes.decide(
            "https://cdn.example.net/npm/kept/a.css",
            Reference::Style,
            false,
            &PAGE
        ),
        Decision::Keep,
        "a stylesheet no [[serve]] entry moves stays where its own addresses resolve"
    );
    assert_eq!(
        routes.decide(
            "https://cdn.example.net/npm/moved/a.css",
            Reference::Style,
            false,
            &PAGE
        ),
        Decision::Rehost(format!(
            "https://reader.example.com/www.publisher.example{PREFIX}moved/a.css"
        ))
    );
}

#[test]
fn a_route_address_is_a_fixed_point() {
    let routes = routes(PhaseEntries::default());
    let emitted = format!("https://reader.example.com/www.publisher.example{PREFIX}a.png");
    assert_eq!(
        routes.decide(&emitted, Reference::Media, false, &PAGE),
        Decision::Keep
    );
}

#[test]
fn an_address_dispatch_would_send_elsewhere_is_kept() {
    // Refused at startup, so this proves the runtime check on its own.
    let mut nested = ProxyAssetRoute::new(format!("{PREFIX}private/"), "https://bucket.example");
    nested.rehost_page_references = false;
    let routes = RehostRoutes::of_parts(
        &[rehosting(PREFIX, "https://cdn.example.net/npm"), nested],
        PhaseEntries::default(),
    );
    assert_eq!(
        routes.decide(
            "https://cdn.example.net/npm/private/a.png",
            Reference::Media,
            false,
            &PAGE
        ),
        Decision::Keep
    );
}

#[test]
fn a_root_origin_maps_without_a_double_slash() {
    let routes = RehostRoutes::of_parts(
        &[rehosting("/_rehost/img/", "https://img.example.net")],
        PhaseEntries::default(),
    );
    assert_eq!(
        routes.decide(
            "https://img.example.net/a/b.png",
            Reference::Media,
            false,
            &PAGE
        ),
        Decision::Rehost(
            "https://reader.example.com/www.publisher.example/_rehost/img/a/b.png".to_owned()
        )
    );
    assert_eq!(
        routes.decide("https://img.example.net", Reference::Media, false, &PAGE),
        Decision::Keep,
        "the bare origin names no file"
    );
}

#[test]
fn srcset_candidates_keep_commas_inside_their_addresses() {
    let value = "https://a.example/x.png?resize=439,277 1x, b.png 2x,c.png";
    let addresses: Vec<&str> = srcset_address_ranges(value)
        .into_iter()
        .map(|range| &value[range])
        .collect();
    assert_eq!(
        addresses,
        ["https://a.example/x.png?resize=439,277", "b.png", "c.png"]
    );
}

#[test]
fn a_rehosting_route_refuses_settings_that_break_its_round_trip() {
    let mut cases: Vec<(&str, ProxyAssetRoute)> = Vec::new();
    let mut pattern = rehosting(PREFIX, "https://cdn.example.net/npm");
    pattern.path_pattern = Some("^/x/(.*)$".to_owned());
    pattern.target_path = Some("/$1".to_owned());
    cases.push(("path_pattern", pattern));
    cases.push((
        "plain http",
        rehosting(PREFIX, "http://cdn.example.net/npm"),
    ));
    cases.push((
        "no closing slash",
        rehosting("/_rehost/cdn", "https://cdn.example.net/npm"),
    ));
    cases.push((
        "namespace",
        rehosting("/_ts/cdn/", "https://cdn.example.net/npm"),
    ));
    for (setting, mut origin) in [
        (
            "host_header",
            OriginSettings::new("https://cdn.example.net/npm"),
        ),
        (
            "directory_index",
            OriginSettings::new("https://cdn.example.net/npm"),
        ),
    ] {
        if setting == "host_header" {
            origin.host_header = Some("other.example.net".to_owned());
        } else {
            origin.directory_index = Some("index.html".to_owned());
        }
        let mut route = rehosting(PREFIX, "https://cdn.example.net/npm");
        route.set_origin_source(&origin);
        cases.push((setting, route));
    }
    for (case, route) in cases {
        assert!(check_route(&route).is_err(), "should refuse {case}");
    }
    let mut same_host = OriginSettings::new("https://cdn.example.net/npm");
    same_host.host_header = Some("cdn.example.net".to_owned());
    let mut route = rehosting(PREFIX, "https://cdn.example.net/npm");
    route.set_origin_source(&same_host);
    assert!(check_route(&route).is_ok(), "the url's own host is allowed");
}

#[test]
fn host_override_must_match_the_complete_http_authority() {
    for (url, host, valid) in [
        ("https://cdn.example.net:8443/npm", "cdn.example.net", false),
        (
            "https://cdn.example.net:8443/npm",
            "cdn.example.net:8443",
            true,
        ),
        (
            "https://cdn.example.net:8443/npm",
            "cdn.example.net:9443",
            false,
        ),
        ("https://cdn.example.net/npm", "CDN.EXAMPLE.NET", true),
        ("https://[2001:db8::1]:8443/npm", "[2001:db8::1]:8443", true),
    ] {
        let mut origin = OriginSettings::new(url);
        origin.host_header = Some(host.to_owned());
        let mut route = rehosting(PREFIX, url);
        route.set_origin_source(&origin);
        assert_eq!(check_route(&route).is_ok(), valid, "{url} with Host {host}");
    }
}

#[test]
fn document_base_resolves_relative_references_and_protocol_relative_scheme() {
    let routes = routes(PhaseEntries::default());
    let base = Url::parse("https://cdn.example.net/npm/lib/page.html").expect("base");
    let page = PageAddress {
        base: Some(&base),
        ..PAGE
    };
    assert_eq!(
        routes.decide("../a.png?q=%2f&x#f", Reference::Media, false, &page),
        Decision::Rehost(format!(
            "https://reader.example.com/www.publisher.example{PREFIX}a.png?q=%2f&x#f"
        )),
    );
    let base = Url::parse("http://cdn.example.net/npm/").expect("base");
    let page = PageAddress {
        base: Some(&base),
        ..PAGE
    };
    assert_eq!(
        routes.decide(
            "//cdn.example.net/npm/a.png",
            Reference::Media,
            false,
            &page
        ),
        Decision::Unmatched,
        "the base's HTTP scheme must not become the reader's HTTPS scheme",
    );
}

#[test]
fn rehosting_routes_must_not_overlap() {
    let valid = [
        rehosting(PREFIX, "https://cdn.example.net/npm"),
        rehosting("/_rehost/img/", "https://img.example.net"),
    ];
    assert!(
        check_routes(&valid).is_ok(),
        "distinct routes make their round trips"
    );

    let overlapping_prefix = [
        rehosting(PREFIX, "https://cdn.example.net/npm"),
        ProxyAssetRoute::new(format!("{PREFIX}x/"), "https://bucket.example"),
    ];
    assert!(check_routes(&overlapping_prefix).is_err());

    let nested_folders = [
        rehosting(PREFIX, "https://cdn.example.net/npm"),
        rehosting("/_rehost/deep/", "https://cdn.example.net/npm/lib"),
    ];
    assert!(check_routes(&nested_folders).is_err());
}
