use trusted_server_js::{bundle_hash, TsjsBundle};

fn script_src_for(bundle: TsjsBundle) -> String {
    format!(
        "/static/tsjs={}?v={}",
        bundle.minified_filename(),
        bundle_hash(bundle)
    )
}

fn script_tag_for(bundle: TsjsBundle, attrs: &str) -> String {
    let attr_segment = if attrs.is_empty() {
        String::new()
    } else {
        format!(" {}", attrs)
    };
    format!(
        "<script src=\"{}\"{}></script>",
        script_src_for(bundle),
        attr_segment
    )
}

/// `/static` URL for the unified bundle with cache-busting hash.
pub fn unified_script_src() -> String {
    script_src_for(TsjsBundle::Unified)
}

/// `<script>` tag for injecting the unified bundle.
pub fn unified_script_tag() -> String {
    script_tag_for(TsjsBundle::Unified, "id=\"trustedserver-js\"")
}
