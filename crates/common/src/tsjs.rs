use trusted_server_js::{all_module_ids, concatenated_hash};

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the concatenated content of the given module set.
#[must_use]
pub fn tsjs_script_src(module_ids: &[&str]) -> String {
    let hash = concatenated_hash(module_ids);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// `<script>` tag for injecting the tsjs bundle.
#[must_use]
pub fn tsjs_script_tag(module_ids: &[&str]) -> String {
    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"></script>",
        tsjs_script_src(module_ids)
    )
}

/// `/static` URL using **all** available modules. Used in contexts that lack
/// an `IntegrationRegistry` (e.g., creative rewriting, config defaults).
#[must_use]
pub fn tsjs_script_src_all() -> String {
    let ids = all_module_ids();
    tsjs_script_src(&ids)
}

/// `<script>` tag using **all** available modules.
#[must_use]
pub fn tsjs_script_tag_all() -> String {
    let ids = all_module_ids();
    tsjs_script_tag(&ids)
}
