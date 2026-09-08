//! Synthesized negative fixtures for the scoped JavaScript documentation rules.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use error_stack::{Report, ResultExt as _};
use serde::Deserialize;

use crate::repository::Repository;

const FIXTURE_DIRECTORY: &str = "tools/docs-parity/tests/fixtures/jsdoc";
const JAVASCRIPT_DIRECTORY: &str = "crates/trusted-server-js/lib";
const MAXIMUM_FIXTURE_BYTES: usize = 64 * 1024;

/// Expected failure for one isolated fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureSpec {
    /// Filename below the checked fixture directory.
    pub file_name: &'static str,
    /// Config-relative path presented to `ESLint` through `--stdin-filename`.
    pub virtual_path: &'static str,
    /// The only rule permitted to report for this fixture.
    pub expected_rule: &'static str,
}

const FIXTURE_SPECS: [FixtureSpec; 10] = [
    FixtureSpec {
        file_name: "file-overview.ts",
        virtual_path: "build-prebid-external.mjs",
        expected_rule: "jsdoc/require-file-overview",
    },
    FixtureSpec {
        file_name: "exported-function.ts",
        virtual_path: "src/core/render.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "exported-class.ts",
        virtual_path: "src/core/registry.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "exported-interface.ts",
        virtual_path: "src/core/types.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "exported-type-alias.ts",
        virtual_path: "src/shared/globals.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "exported-variable.ts",
        virtual_path: "src/integrations/creative/exported-variable.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "default-export.ts",
        virtual_path: "src/integrations/creative/default-export.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "re-export.ts",
        virtual_path: "src/integrations/creative/re-export.ts",
        expected_rule: "jsdoc/require-jsdoc",
    },
    FixtureSpec {
        file_name: "alignment.ts",
        virtual_path: "src/integrations/creative/alignment.ts",
        expected_rule: "jsdoc/check-alignment",
    },
    FixtureSpec {
        file_name: "types.ts",
        virtual_path: "src/integrations/creative/types.ts",
        expected_rule: "jsdoc/check-types",
    },
];

/// Return the exact fixture-to-rule contract.
#[must_use]
pub const fn fixture_specs() -> &'static [FixtureSpec] {
    &FIXTURE_SPECS
}

/// Validate exact fixture-set equality and bounded UTF-8 contents.
///
/// # Errors
///
/// Returns an error for a missing, extra, empty, oversized, or non-UTF-8 fixture.
pub fn validate_fixture_inventory(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), Report<JsdocError>> {
    let expected = FIXTURE_SPECS
        .iter()
        .map(|spec| spec.file_name)
        .collect::<BTreeSet<_>>();
    let actual = files.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(Report::new(JsdocError).attach(format!(
            "fixture set differs: expected {expected:?}, found {actual:?}"
        )));
    }
    for (name, bytes) in files {
        if bytes.is_empty() || bytes.len() > MAXIMUM_FIXTURE_BYTES {
            return Err(Report::new(JsdocError).attach(format!(
                "fixture `{name}` must contain 1..={MAXIMUM_FIXTURE_BYTES} bytes"
            )));
        }
        core::str::from_utf8(bytes)
            .change_context(JsdocError)
            .attach_with(|| format!("fixture `{name}` is not UTF-8"))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EslintReport {
    messages: Vec<EslintMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EslintMessage {
    rule_id: Option<String>,
    severity: u8,
}

/// Check that each tracked fixture fails through exactly its expected rule.
///
/// # Errors
///
/// Returns an error when the fixture inventory is not exact, `ESLint` cannot run,
/// or a fixture passes or reports any rule other than its declared failure.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<JsdocError>> {
    let prefix = format!("{FIXTURE_DIRECTORY}/");
    let mut files = BTreeMap::new();
    for path in repository.tracked_paths().change_context(JsdocError)? {
        let text = path.as_utf8().change_context(JsdocError)?;
        if let Some(name) = text.strip_prefix(&prefix) {
            if name.contains('/') {
                return Err(Report::new(JsdocError)
                    .attach(format!("nested JSDoc fixture is not allowed: {text}")));
            }
            let bytes = repository
                .read_tracked_bounded(&path, MAXIMUM_FIXTURE_BYTES)
                .change_context(JsdocError)?;
            files.insert(name.to_owned(), bytes);
        }
    }
    validate_fixture_inventory(&files)?;

    let javascript_directory = repository.root().join(JAVASCRIPT_DIRECTORY);
    let eslint_script = javascript_directory.join("node_modules/eslint/bin/eslint.js");
    if !eslint_script.is_file() {
        return Err(Report::new(JsdocError).attach(format!(
            "ESLint is not installed at {}",
            eslint_script.display()
        )));
    }

    for spec in FIXTURE_SPECS {
        let bytes = files
            .get(spec.file_name)
            .expect("validated fixture inventory should contain every specification");
        run_fixture(&javascript_directory, &eslint_script, spec, bytes)?;
    }
    for virtual_path in [
        "src/generated/jsdoc-control.ts",
        "src/integrations/prebid/jsdoc-control.ts",
        "vendor/jsdoc-control.ts",
    ] {
        run_unscoped_control(&javascript_directory, &eslint_script, virtual_path)?;
    }
    Ok(())
}

fn run_fixture(
    javascript_directory: &Path,
    eslint_script: &Path,
    spec: FixtureSpec,
    bytes: &[u8],
) -> Result<(), Report<JsdocError>> {
    let output = run_eslint(
        javascript_directory,
        eslint_script,
        spec.virtual_path,
        bytes,
    )?;
    if output.status.code() != Some(1) {
        return Err(Report::new(JsdocError)
            .attach(format!("fixture `{}` did not fail lint", spec.file_name))
            .attach(format!("status: {}", output.status))
            .attach(output.stderr));
    }
    let messages = output
        .reports
        .iter()
        .flat_map(|report| report.messages.iter())
        .collect::<Vec<_>>();
    if messages.is_empty()
        || messages.iter().any(|message| {
            message.severity != 2 || message.rule_id.as_deref() != Some(spec.expected_rule)
        })
    {
        let rules = messages
            .iter()
            .map(|message| (message.rule_id.as_deref(), message.severity))
            .collect::<Vec<_>>();
        return Err(Report::new(JsdocError).attach(format!(
            "fixture `{}` expected only `{}`, found {rules:?}",
            spec.file_name, spec.expected_rule
        )));
    }
    Ok(())
}

fn run_unscoped_control(
    javascript_directory: &Path,
    eslint_script: &Path,
    virtual_path: &str,
) -> Result<(), Report<JsdocError>> {
    const UNDOCUMENTED_EXPORT: &[u8] = b"export function jsdocControl(): number { return 1; }\n";
    let output = run_eslint(
        javascript_directory,
        eslint_script,
        virtual_path,
        UNDOCUMENTED_EXPORT,
    )?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(Report::new(JsdocError)
            .attach(format!(
                "unscoped control `{virtual_path}` could not be linted"
            ))
            .attach(format!("status: {}", output.status))
            .attach(output.stderr));
    }
    let jsdoc_rules = output
        .reports
        .iter()
        .flat_map(|report| report.messages.iter())
        .filter_map(|message| message.rule_id.as_deref())
        .filter(|rule| rule.starts_with("jsdoc/"))
        .collect::<Vec<_>>();
    if !jsdoc_rules.is_empty() {
        return Err(Report::new(JsdocError).attach(format!(
            "unscoped control `{virtual_path}` activated JSDoc rules: {jsdoc_rules:?}"
        )));
    }
    Ok(())
}

struct EslintOutput {
    status: ExitStatus,
    reports: Vec<EslintReport>,
    stderr: String,
}

fn run_eslint(
    javascript_directory: &Path,
    eslint_script: &Path,
    virtual_path: &str,
    bytes: &[u8],
) -> Result<EslintOutput, Report<JsdocError>> {
    let mut child = Command::new("node")
        .arg(eslint_script)
        .args([
            "--stdin",
            "--stdin-filename",
            virtual_path,
            "--format",
            "json",
        ])
        .current_dir(javascript_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .change_context(JsdocError)
        .attach_with(|| format!("start ESLint for `{virtual_path}`"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| Report::new(JsdocError).attach("ESLint stdin was not piped"))?
        .write_all(bytes)
        .change_context(JsdocError)
        .attach_with(|| format!("write ESLint input for `{virtual_path}`"))?;
    let output = child
        .wait_with_output()
        .change_context(JsdocError)
        .attach_with(|| format!("wait for ESLint input `{virtual_path}`"))?;
    let reports: Vec<EslintReport> = serde_json::from_slice(&output.stdout)
        .change_context(JsdocError)
        .attach_with(|| format!("parse ESLint JSON for `{virtual_path}`"))?;
    Ok(EslintOutput {
        status: output.status,
        reports,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[derive(Debug, derive_more::Display)]
#[display("JSDoc fixture validation failed")]
pub struct JsdocError;

impl core::error::Error for JsdocError {}
