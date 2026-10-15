//! Closed compatibility for authenticated, exact-version npm originals.
use super::{field, material_error};
use crate::error::ReleaseError;
use error_stack::Report;
use std::collections::BTreeMap;

pub(super) fn timestamp_prefix(header: &tar::Header) -> Result<&str, Report<ReleaseError>> {
    let bytes = header.as_bytes();
    if &bytes[257..265] != b"ustar\x0000"
        || bytes[345..476].iter().any(|byte| *byte != 0)
        || bytes[500..].iter().any(|byte| *byte != 0)
        || [476, 488].iter().any(|start| {
            bytes[*start..*start + 11]
                .iter()
                .any(|byte| !matches!(byte, b'0'..=b'7'))
                || bytes[*start + 11] != 0
        })
    {
        return Err(material_error("invalid reviewed node-tar timestamp layout"));
    }
    field(&bytes[345..475])
}

pub(super) struct LocalPax {
    path: String,
    size: u64,
}
impl LocalPax {
    pub(super) fn header_name(&self) -> String {
        format!("PaxHeader/{}", self.path)
    }
    pub(super) fn join(&self, path: &str, size: u64) -> Result<(), Report<ReleaseError>> {
        if self.path != path || self.size != size {
            return Err(material_error(
                "local PAX path or size would override raw member",
            ));
        }
        Ok(())
    }
}
const KEYS: &[&str] = &[
    "NODETAR.blksize",
    "NODETAR.blocks",
    "NODETAR.depth",
    "NODETAR.follow",
    "NODETAR.ignoreFiles.0",
    "NODETAR.ignoreFiles.1",
    "NODETAR.ignoreFiles.2",
    "NODETAR.package.author.email",
    "NODETAR.package.author.name",
    "NODETAR.package.author.url",
    "NODETAR.package.contributors.0.email",
    "NODETAR.package.contributors.0.name",
    "NODETAR.package.contributors.0.url",
    "NODETAR.package.dependencies.consolidate",
    "NODETAR.package.dependencies.es6-promise",
    "NODETAR.package.dependencies.fs-readfile-promise",
    "NODETAR.package.dependencies.js-yaml",
    "NODETAR.package.dependencies.lodash",
    "NODETAR.package.dependencies.node.extend",
    "NODETAR.package.dependencies.plugin-error",
    "NODETAR.package.dependencies.through2",
    "NODETAR.package.dependencies.tryit",
    "NODETAR.package.dependencies.vinyl-bufferstream",
    "NODETAR.package.description",
    "NODETAR.package.devDependencies.eslint",
    "NODETAR.package.devDependencies.expect.js",
    "NODETAR.package.devDependencies.istanbul",
    "NODETAR.package.devDependencies.istanbul-coveralls",
    "NODETAR.package.devDependencies.jscs",
    "NODETAR.package.devDependencies.mocha",
    "NODETAR.package.devDependencies.simple-bufferstream",
    "NODETAR.package.devDependencies.vinyl",
    "NODETAR.package.engines.node",
    "NODETAR.package.engines.npm",
    "NODETAR.package.files.0",
    "NODETAR.package.jscsConfig.maximumLineLength",
    "NODETAR.package.jscsConfig.preset",
    "NODETAR.package.jscsConfig.requireBlocksOnNewline",
    "NODETAR.package.jscsConfig.validateLineBreaks",
    "NODETAR.package.keywords.0",
    "NODETAR.package.keywords.1",
    "NODETAR.package.keywords.2",
    "NODETAR.package.keywords.3",
    "NODETAR.package.keywords.4",
    "NODETAR.package.keywords.5",
    "NODETAR.package.licenses.0.type",
    "NODETAR.package.licenses.0.url",
    "NODETAR.package.name",
    "NODETAR.package.repository",
    "NODETAR.package.scripts.coverage",
    "NODETAR.package.scripts.coveralls",
    "NODETAR.package.scripts.pretest",
    "NODETAR.package.scripts.test",
    "NODETAR.package.version",
    "NODETAR.type",
    "SCHILY.dev",
    "SCHILY.ino",
    "SCHILY.nlink",
    "gid",
    "path",
    "size",
    "uid",
];
pub(super) fn parse_pax(mut bytes: &[u8]) -> Result<LocalPax, Report<ReleaseError>> {
    let mut values = BTreeMap::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| material_error("invalid PAX length framing"))?;
        let digits = std::str::from_utf8(&bytes[..space]).map_err(material_error)?;
        if digits.is_empty()
            || digits.starts_with('0')
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(material_error("noncanonical PAX length"));
        }
        let length = digits.parse::<usize>().map_err(material_error)?;
        if length <= space + 2 || length > bytes.len() || bytes[length - 1] != b'\n' {
            return Err(material_error("invalid PAX record length"));
        }
        let record = std::str::from_utf8(&bytes[space + 1..length - 1]).map_err(material_error)?;
        let (key, value) = record
            .split_once('=')
            .ok_or_else(|| material_error("invalid PAX assignment"))?;
        if !KEYS.contains(&key)
            || value.contains(['\0', '\n', '\r'])
            || values.insert(key, value).is_some()
        {
            return Err(material_error("unknown or repeated PAX metadata field"));
        }
        bytes = &bytes[length..];
    }
    let path = values
        .get("path")
        .ok_or_else(|| material_error("PAX lacks path"))?
        .to_string();
    let size = values
        .get("size")
        .ok_or_else(|| material_error("PAX lacks size"))?;
    if size.is_empty()
        || (size.len() > 1 && size.starts_with('0'))
        || !size.bytes().all(|byte| byte.is_ascii_digit())
        || values.get("NODETAR.type") != Some(&"File")
    {
        return Err(material_error("invalid PAX size or member type"));
    }
    Ok(LocalPax {
        path,
        size: size.parse().map_err(material_error)?,
    })
}
