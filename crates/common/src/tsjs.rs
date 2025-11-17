use trusted_server_js::{bundle_for_filename, bundle_hash};

fn script_src_internal(filename: &str) -> String {
    script_src(filename).unwrap_or_else(|| {
        panic!(
            "tsjs: bundle {} not found. Ensure npm build produced this bundle.",
            filename
        )
    })
}

fn script_tag_internal(filename: &str, attrs: &str) -> String {
    let attr_segment = if attrs.is_empty() {
        String::new()
    } else {
        format!(" {}", attrs)
    };
    format!(
        "<script src=\"{}\"{}></script>",
        script_src_internal(filename),
        attr_segment
    )
}

/// Returns `/static/tsjs=<filename>?v=<hash>` if the bundle is available.
pub fn script_src(filename: &str) -> Option<String> {
    let trimmed = filename.trim();
    bundle_for_filename(trimmed)?;
    let hash = bundle_hash(trimmed)?;
    let minified = trimmed
        .strip_suffix(".js")
        .map(|stem| format!("{stem}.min.js"))
        .unwrap_or_else(|| format!("{trimmed}.min.js"));
    Some(format!("/static/tsjs={}?v={}", minified, hash))
}

/// Returns a `<script>` tag referencing the provided bundle if available.
pub fn script_tag(filename: &str, attrs: &str) -> Option<String> {
    script_src(filename).map(|src| {
        if attrs.is_empty() {
            format!("<script src=\"{}\"></script>", src)
        } else {
            format!("<script src=\"{}\" {}></script>", src, attrs)
        }
    })
}

/// `<script>` tag for injecting the core runtime.
pub fn core_script_tag() -> String {
    script_tag_internal("tsjs-core.js", "id=\"trustedserver-js\"")
}

/// `<script>` tag for injecting the creative guard runtime.
pub fn creative_script_tag() -> String {
    script_tag_internal("tsjs-creative.js", "async")
}

/// `/static` URL for the core bundle with cache-busting hash.
pub fn core_script_src() -> String {
    script_src_internal("tsjs-core.js")
}

/// `/static` URL for the extension bundle with cache-busting hash.
pub fn ext_script_src() -> String {
    script_src_internal("tsjs-ext.js")
}

/// `/static` URL for the creative bundle with cache-busting hash.
pub fn creative_script_src() -> String {
    script_src_internal("tsjs-creative.js")
}
