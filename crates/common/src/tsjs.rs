use trusted_server_js::{bundle_for_filename, bundle_hash};

#[cfg(test)]
use once_cell::sync::Lazy;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::RwLock;

#[cfg(test)]
static MOCK_INTEGRATION_BUNDLES: Lazy<RwLock<HashMap<String, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

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

fn integration_bundle_filename(name: &str) -> String {
    format!("tsjs-{name}.js")
}

fn integration_fallback_src(name: &str) -> String {
    format!("/static/tsjs=tsjs-{name}.min.js")
}

/// Returns the script URL for an integration bundle, falling back to the
/// static path when the hashed bundle isn't available (e.g., in tests).
pub fn integration_script_src(name: &str) -> String {
    #[cfg(test)]
    {
        if let Some(src) = MOCK_INTEGRATION_BUNDLES
            .read()
            .expect("mock bundle lock should not be poisoned")
            .get(name)
            .cloned()
        {
            return src;
        }
    }
    script_src(&integration_bundle_filename(name)).unwrap_or_else(|| integration_fallback_src(name))
}

/// Returns a `<script>` tag for the integration bundle, falling back to a
/// plain static tag if the hashed bundle isn't available.
pub fn integration_script_tag(name: &str, attrs: &str) -> String {
    let src = integration_script_src(name);
    if attrs.is_empty() {
        format!("<script src=\"{}\"></script>", src)
    } else {
        format!("<script src=\"{}\" {}></script>", src, attrs)
    }
}

#[cfg(test)]
pub fn mock_integration_bundle(name: &str, src: impl Into<String>) {
    MOCK_INTEGRATION_BUNDLES
        .write()
        .expect("mock bundle lock should not be poisoned")
        .insert(name.to_string(), src.into());
}

#[cfg(test)]
pub fn clear_mock_integration_bundles() {
    MOCK_INTEGRATION_BUNDLES
        .write()
        .expect("mock bundle lock should not be poisoned")
        .clear();
}
