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

/// `<script>` tag for injecting the core runtime.
pub fn core_script_tag() -> String {
    script_tag_for(TsjsBundle::Core, "id=\"trustedserver-js\"")
}

/// `<script>` tag for injecting the creative guard runtime.
pub fn creative_script_tag() -> String {
    script_tag_for(TsjsBundle::Creative, "async")
}

/// `/static` URL for the core bundle with cache-busting hash.
pub fn core_script_src() -> String {
    script_src_for(TsjsBundle::Core)
}

/// `/static` URL for the extension bundle with cache-busting hash.
pub fn ext_script_src() -> String {
    script_src_for(TsjsBundle::Ext)
}

/// `/static` URL for the creative bundle with cache-busting hash.
pub fn creative_script_src() -> String {
    script_src_for(TsjsBundle::Creative)
}
