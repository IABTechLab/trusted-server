//! Final-byte path checks derived from the bound recipe rather than callers.

use std::path::Path;

use error_stack::Report;

use super::paths::execution_text;
use super::remap::scan_remapped_bytes;
use super::{ExecutionPaths, PreparedRecipe, recipe_error};
use crate::error::ReleaseError;

/// Requires a bound host root to be strict absolute UTF-8 text.
fn host_text(path: &Path) -> Result<String, Report<ReleaseError>> {
    path.to_str()
        .filter(|text| path.is_absolute() && !text.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| recipe_error("a final-path root is not plain absolute UTF-8"))
}

impl PreparedRecipe {
    /// Rejects every host and execution root and requires a fixed remapped prefix.
    ///
    /// The six mounted roots contribute both names. Output is host-only and
    /// never translated because the build cannot access it. Whole material and
    /// Cargo-state roots include vendor files, HOME, TMPDIR and Cargo home. A
    /// Darwin build's Apple developer directory and detached SDK contribute
    /// both names too.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid root text, failed translation, a remaining
    /// root or `/boundary/` prefix, or no fixed remapped prefix in the bytes.
    pub(super) fn check_final_paths(
        &self,
        bytes: &[u8],
        paths: &dyn ExecutionPaths,
    ) -> Result<(), Report<ReleaseError>> {
        let roots = [
            self.ambient.checkout_root(),
            self.retained_root.as_path(),
            self.request.materials.as_path(),
            self.tools.root(),
            self.request.cargo_state.as_path(),
            self.request.target_dir.as_path(),
        ];
        // Six host roots, six execution roots, the host output and a Darwin
        // build's developer directory and detached SDK under both names stay
        // below the existing scanner's fixed limit of sixty-four prefixes.
        let mut forbidden = Vec::with_capacity(17);
        for root in roots {
            forbidden.push(host_text(root)?);
            forbidden.push(execution_text(paths, "final-path root", root)?);
        }
        forbidden.push(host_text(&self.request.output)?);
        if let Some(apple) = self.tools.apple() {
            let developer = apple.developer_dir();
            forbidden.push(host_text(developer)?);
            forbidden.push(execution_text(
                paths,
                "Apple developer directory",
                developer,
            )?);
            if let Some(sdk) = apple.owned_sdk_root() {
                forbidden.push(host_text(sdk)?);
                forbidden.push(execution_text(paths, "owned Apple SDK", sdk)?);
            }
        }
        let forbidden: Vec<&str> = forbidden.iter().map(String::as_str).collect();
        scan_remapped_bytes(bytes, &forbidden)?;
        Ok(())
    }
}
