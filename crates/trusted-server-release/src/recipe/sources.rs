//! Cargo source-replacement arguments for the vendored dependency tree.

use crate::materials::GitSourceConfig;

/// Name of the directory source every locked source is replaced with.
pub(super) const VENDOR_SOURCE: &str = "ts-vendor";

/// Returns the `--config` pairs replacing every locked source with the vendor
/// directory.
///
/// `vendor_directory` is the directory as the confined build sees it and must
/// already be plain text without quotes or backslashes. Each Git source is a
/// full `[source."<key>"]` table (`git`, reference, `replace-with`), because
/// Cargo rejects a source table without a location.
pub(super) fn source_replacement_arguments(
    vendor_directory: &str,
    git_sources: &[GitSourceConfig],
) -> Vec<String> {
    let mut entries = vec![format!("source.crates-io.replace-with=\"{VENDOR_SOURCE}\"")];
    for source in git_sources {
        let key = source.key();
        entries.push(format!("source.\"{key}\".git=\"{}\"", source.url()));
        if let Some((kind, value)) = source.reference() {
            entries.push(format!("source.\"{key}\".{kind}=\"{value}\""));
        }
        entries.push(format!("source.\"{key}\".replace-with=\"{VENDOR_SOURCE}\""));
    }
    entries.push(format!(
        "source.{VENDOR_SOURCE}.directory=\"{vendor_directory}\""
    ));
    entries
        .into_iter()
        .flat_map(|entry| ["--config".to_owned(), entry])
        .collect()
}
