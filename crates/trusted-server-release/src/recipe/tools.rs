//! The Rust toolchain a recipe builds with, and how its selection is gated.

use std::fs;
use std::path::{Path, PathBuf};

use error_stack::Report;

use super::fresh::{real_directory, real_file};
use super::recipe_error;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::tool_tree::tool_tree_digest;

/// A Rust toolchain that passed Task 8 qualification.
///
/// None has, so this type is uninhabited and production selection stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedRustTools {}

/// Whether a selection carries release authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Authority {
    /// A qualified toolchain; cannot be constructed while none is qualified.
    Qualified,
    /// An ordinary CI toolchain that proves nothing about release provenance.
    Unqualified,
}

/// How a toolchain was selected.
///
/// Production builds can only name [`QualifiedRustTools`], which is
/// uninhabited, so no production value exists until a toolchain is qualified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSelection {
    /// A qualified production selection.
    Qualified(QualifiedRustTools),
    /// An ordinary CI toolchain; test builds only.
    #[cfg(test)]
    OrdinaryCi,
}

impl ToolSelection {
    /// Returns the authority this selection carries.
    pub(crate) fn authority(&self) -> Authority {
        match self {
            Self::Qualified(qualification) => match *qualification {},
            #[cfg(test)]
            Self::OrdinaryCi => Authority::Unqualified,
        }
    }

    /// Reports whether this selection may start an unqualified candidate run.
    ///
    /// Only the test-only ordinary CI selection may, so a candidate run is
    /// evidence for CI and never release qualification. A production build
    /// can name no selection at all, so its execute gate stays closed.
    pub(crate) fn permits_candidate_run(&self) -> bool {
        match self {
            Self::Qualified(qualification) => match *qualification {},
            #[cfg(test)]
            Self::OrdinaryCi => true,
        }
    }
}

/// Fixed `bin/cargo`, `bin/rustc` and target standard-library layout, retained
/// as a measured candidate.
pub(crate) struct SelectedRustTools {
    root: PathBuf,
    target: ReleaseTarget,
    selection: ToolSelection,
}

/// Requires the fixed layout for `target` below the canonical `root`.
///
/// The target's `lib/rustlib/<triple>/lib` directory must hold a `libstd-*`
/// file, so a toolchain installed without the target's standard library is
/// rejected before any build starts.
fn check_layout(root: &Path, target: ReleaseTarget) -> Result<(), Report<ReleaseError>> {
    for relative in ["bin/cargo", "bin/rustc"] {
        real_file("Rust tool", &root.join(relative))?;
    }
    let standard = root.join("lib/rustlib").join(target.triple()).join("lib");
    real_directory("Rust target standard library directory", &standard)?;
    let list_error = |error: std::io::Error| {
        recipe_error(format!("cannot list the target standard library: {error}"))
    };
    let mut has_std = false;
    for entry in fs::read_dir(&standard).map_err(list_error)? {
        let entry = entry.map_err(list_error)?;
        let kind = entry.file_type().map_err(list_error)?;
        has_std |= kind.is_file() && entry.file_name().to_string_lossy().starts_with("libstd-");
    }
    if !has_std {
        return Err(recipe_error(format!(
            "Rust tools hold no libstd file for target {}",
            target.triple()
        )));
    }
    Ok(())
}

impl SelectedRustTools {
    /// Accepts only a qualified selection; production construction stays closed.
    pub(crate) fn qualified(_root: &Path, qualification: QualifiedRustTools) -> Self {
        match qualification {}
    }

    /// Selects ordinary CI tools without asserting release qualification.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] unless `bin/cargo` and `bin/rustc`
    /// are regular files and `lib/rustlib/<triple>/lib` holds a `libstd-*`
    /// file, all below the canonical `root`.
    #[cfg(test)]
    pub(crate) fn for_tests(
        root: &Path,
        target: ReleaseTarget,
    ) -> Result<Self, Report<ReleaseError>> {
        let root = fs::canonicalize(root)
            .map_err(|error| recipe_error(format!("cannot resolve Rust tools: {error}")))?;
        check_layout(&root, target)?;
        Ok(Self {
            root,
            target,
            selection: ToolSelection::OrdinaryCi,
        })
    }

    /// The canonical tool root.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The target whose standard library the layout was checked for.
    pub(crate) fn target(&self) -> ReleaseTarget {
        self.target
    }

    /// The authority of this selection.
    pub(crate) fn authority(&self) -> Authority {
        self.selection.authority()
    }

    /// Whether this selection may start an unqualified candidate run.
    pub(crate) fn permits_candidate_run(&self) -> bool {
        self.selection.permits_candidate_run()
    }

    /// Host path of the selected Cargo.
    pub(crate) fn cargo(&self) -> PathBuf {
        self.root.join("bin/cargo")
    }

    /// Host path of the selected compiler.
    pub(crate) fn rustc(&self) -> PathBuf {
        self.root.join("bin/rustc")
    }

    /// Host path of the directory holding both tools.
    pub(crate) fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// Digest of the whole tool tree: contents, modes, names and aliases.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for a hard-linked or special file, an
    /// alias that escapes the root, or an exceeded limit.
    pub(crate) fn digest(&self) -> Result<String, Report<ReleaseError>> {
        tool_tree_digest(&self.root, &|| Ok(())).map_err(|error| {
            error
                .change_context(ReleaseError::Recipe)
                .attach("Rust tool tree cannot be digested")
        })
    }
}
