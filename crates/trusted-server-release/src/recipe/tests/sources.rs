use super::super::sources::source_replacement_arguments;
use crate::materials::GitSourceConfig;

fn parse(selector: &str) -> GitSourceConfig {
    GitSourceConfig::parse(selector).expect("should parse the fictional selector")
}

#[test]
fn replacement_argv_is_exact_for_registry_only() {
    assert_eq!(
        source_replacement_arguments("/boundary/immutable/2/vendor", &[]),
        [
            "--config",
            "source.crates-io.replace-with=\"ts-vendor\"",
            "--config",
            "source.ts-vendor.directory=\"/boundary/immutable/2/vendor\"",
        ],
        "should replace only crates.io when no Git source is locked"
    );
}

#[test]
fn replacement_argv_is_exact_for_every_git_reference_kind() {
    let sources = [
        parse("git+https://example.com/org/tagged?tag=v1.2.3"),
        parse("git+https://example.com/org/branched?branch=release-1"),
        parse("git+https://example.com/org/pinned?rev=abc123"),
        parse("git+https://example.com/org/default"),
    ];
    let expected = [
        "--config",
        "source.crates-io.replace-with=\"ts-vendor\"",
        "--config",
        "source.\"git+https://example.com/org/tagged?tag=v1.2.3\".git=\"https://example.com/org/tagged\"",
        "--config",
        "source.\"git+https://example.com/org/tagged?tag=v1.2.3\".tag=\"v1.2.3\"",
        "--config",
        "source.\"git+https://example.com/org/tagged?tag=v1.2.3\".replace-with=\"ts-vendor\"",
        "--config",
        "source.\"git+https://example.com/org/branched?branch=release-1\".git=\"https://example.com/org/branched\"",
        "--config",
        "source.\"git+https://example.com/org/branched?branch=release-1\".branch=\"release-1\"",
        "--config",
        "source.\"git+https://example.com/org/branched?branch=release-1\".replace-with=\"ts-vendor\"",
        "--config",
        "source.\"git+https://example.com/org/pinned?rev=abc123\".git=\"https://example.com/org/pinned\"",
        "--config",
        "source.\"git+https://example.com/org/pinned?rev=abc123\".rev=\"abc123\"",
        "--config",
        "source.\"git+https://example.com/org/pinned?rev=abc123\".replace-with=\"ts-vendor\"",
        "--config",
        "source.\"git+https://example.com/org/default\".git=\"https://example.com/org/default\"",
        "--config",
        "source.\"git+https://example.com/org/default\".replace-with=\"ts-vendor\"",
        "--config",
        "source.ts-vendor.directory=\"/boundary/immutable/2/vendor\"",
    ];
    assert_eq!(
        source_replacement_arguments("/boundary/immutable/2/vendor", &sources),
        expected,
        "should spell a complete source table for every Git reference kind"
    );
}

#[test]
fn every_config_value_parses_as_a_toml_assignment() {
    let sources = [parse("git+https://example.com/org/tagged?tag=v1.2.3")];
    let arguments = source_replacement_arguments("/boundary/immutable/2/vendor", &sources);
    let entries: Vec<&String> = arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, entry)| entry)
        .collect();
    assert_eq!(
        entries.len() * 2,
        arguments.len(),
        "should pair every value with --config"
    );
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            arguments[index * 2],
            "--config",
            "should lead every value with --config"
        );
        let table: toml::Table = toml::from_str(entry).expect("should parse as a TOML assignment");
        assert_eq!(table.len(), 1, "should assign one top-level table");
        assert!(
            table.contains_key("source"),
            "should configure only the source table: {entry}"
        );
    }
}

#[test]
fn tracked_lockfile_sources_yield_complete_tables() {
    let lock = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("should read the tracked Cargo.lock");
    let text = std::str::from_utf8(&lock).expect("should be UTF-8");
    let selectors: std::collections::BTreeSet<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("source = \"git+"))
        .filter_map(|rest| rest.strip_suffix('"'))
        .filter_map(|rest| {
            rest.rsplit_once('#')
                .map(|(selector, _)| format!("git+{selector}"))
        })
        .collect();
    assert!(
        !selectors.is_empty(),
        "should find a nonzero number of locked Git sources"
    );
    let sources: Vec<GitSourceConfig> = selectors.iter().map(|selector| parse(selector)).collect();
    let arguments = source_replacement_arguments("/boundary/immutable/2/vendor", &sources);
    for source in &sources {
        for needle in [
            format!("source.\"{}\".git=\"{}\"", source.key(), source.url()),
            format!("source.\"{}\".replace-with=\"ts-vendor\"", source.key()),
        ] {
            assert!(
                arguments.contains(&needle),
                "should spell {needle} for the tracked lock"
            );
        }
    }
}
