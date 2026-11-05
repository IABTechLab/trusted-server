//! A prefix-table translator for tests that never touch a real boundary.

use std::path::{Path, PathBuf};

use error_stack::Report;

use super::ExecutionPaths;
use crate::error::ReleaseError;
use crate::recipe::recipe_error;

/// One mapped root: host prefix, execution prefix and writability.
#[derive(Clone, Debug)]
pub(crate) struct MappedRoot {
    host: PathBuf,
    execution: PathBuf,
    writable: bool,
}

/// Translates through an explicit table of mapped roots.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrefixPaths {
    roots: Vec<MappedRoot>,
}

impl PrefixPaths {
    /// Creates an empty table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds a root mapped from `host` to `execution`.
    #[must_use]
    pub(crate) fn with(mut self, host: &Path, execution: &str, writable: bool) -> Self {
        self.roots.push(MappedRoot {
            host: host.to_owned(),
            execution: PathBuf::from(execution),
            writable,
        });
        self
    }
}

impl ExecutionPaths for PrefixPaths {
    fn execution_path(&self, host: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        self.roots
            .iter()
            .find_map(|root| {
                host.strip_prefix(&root.host).ok().map(|relative| {
                    if relative.as_os_str().is_empty() {
                        root.execution.clone()
                    } else {
                        root.execution.join(relative)
                    }
                })
            })
            .ok_or_else(|| recipe_error(format!("{} is outside the fake roots", host.display())))
    }

    fn host_path(&self, execution: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        let normal = execution.components().collect::<PathBuf>().as_os_str()
            == execution.as_os_str()
            && !execution
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir));
        if !normal {
            return Err(recipe_error(format!(
                "{} is not in lexical normal form",
                execution.display()
            )));
        }
        self.roots
            .iter()
            .filter(|root| root.writable)
            .find_map(|root| {
                execution
                    .strip_prefix(&root.execution)
                    .ok()
                    .map(|relative| {
                        if relative.as_os_str().is_empty() {
                            root.host.clone()
                        } else {
                            root.host.join(relative)
                        }
                    })
            })
            .ok_or_else(|| {
                recipe_error(format!(
                    "{} is outside the fake writable roots",
                    execution.display()
                ))
            })
    }
}
