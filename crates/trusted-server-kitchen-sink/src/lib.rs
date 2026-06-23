//! Embedded static assets for the Trusted Server kitchen-sink fixture.

/// A single embedded kitchen-sink asset.
#[derive(Debug)]
pub struct KitchenSinkAsset {
    /// Site-relative path, such as `index.html` or `assets/app.js`.
    pub path: &'static str,
    /// Embedded file bytes.
    pub body: &'static [u8],
    /// HTTP `Content-Type` value inferred at build time.
    pub content_type: &'static str,
    /// Strong content hash suitable for an HTTP `ETag` header.
    pub etag: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/kitchen_sink_assets.rs"));

/// Returns an embedded asset by site-relative path.
#[must_use]
pub fn asset_for_path(path: &str) -> Option<&'static KitchenSinkAsset> {
    ASSETS.iter().find(|asset| asset.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_is_embedded() {
        let asset = asset_for_path("index.html").expect("should embed index.html");

        assert_eq!(
            asset.content_type, "text/html; charset=utf-8",
            "should infer HTML content type"
        );
        assert!(
            asset.body.starts_with(b"<!doctype html>"),
            "should embed index HTML bytes"
        );
    }

    #[test]
    fn common_assets_have_expected_content_types() {
        let css = asset_for_path("assets/styles.css").expect("should embed stylesheet");
        let js = asset_for_path("assets/app.js").expect("should embed app JavaScript");

        assert_eq!(css.content_type, "text/css; charset=utf-8");
        assert_eq!(js.content_type, "application/javascript; charset=utf-8");
    }

    #[test]
    fn dotfiles_are_not_embedded() {
        assert!(
            asset_for_path(".DS_Store").is_none(),
            "should exclude root dotfiles"
        );
        assert!(
            asset_for_path("assets/.DS_Store").is_none(),
            "should exclude nested dotfiles"
        );
    }

    #[test]
    fn missing_asset_returns_none() {
        assert!(asset_for_path("missing.html").is_none());
    }
}
