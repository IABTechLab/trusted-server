//! Guards that adapter crates reach identity only through the core identity app.
//!
//! Adapter crates supply platform services; the identity routes, the partner
//! registry and the EC lifecycle live in `trusted_server_core::ec::app`. This
//! test scans every adapter's non-test source for direct uses of identity
//! entry points. Uses that the EC lifecycle move has not yet taken into core
//! are listed in [`PENDING_LIFECYCLE_MOVE`]; an entry that no longer matches
//! fails the test so the list only shrinks.

use std::fs;
use std::path::{Path, PathBuf};

const ADAPTER_CRATES: &[&str] = &[
    "trusted-server-adapter-axum",
    "trusted-server-adapter-cloudflare",
    "trusted-server-adapter-fastly",
    "trusted-server-adapter-spin",
];

const IDENTITY_ENTRY_POINTS: &[&str] = &[
    "PartnerRegistry::from_config",
    "PartnerRegistry::from_ec",
    "answer_batch_sync_locally",
    "answer_identify_locally",
    "commit_after_response_lookups",
    "ec_finalize_response",
    "generate_if_needed",
    "handle_batch_sync",
    "handle_identify",
];

/// Adapter uses that remain until the EC lifecycle and the identify and
/// batch-sync routes move into the core identity app, as `(file, symbol)`.
const PENDING_LIFECYCLE_MOVE: &[(&str, &str)] = &[];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("should have a crates directory")
        .to_path_buf()
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("should read adapter source directory");
    for entry in entries {
        let path = entry.expect("should read directory entry").path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// Returns `source` with comments removed and string and character literal
/// contents blanked, keeping every other character in place.
///
/// Line comments, nested block comments, escaped and raw strings are handled,
/// so a `//` inside a string literal does not hide the code after it.
fn strip_comments_and_literals(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();
        if current == '/' && next == Some('/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
        } else if current == '/' && next == Some('*') {
            let mut depth = 0_usize;
            while index < chars.len() {
                if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
                    depth += 1;
                    index += 2;
                } else if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                    depth -= 1;
                    index += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[index] == '\n' {
                        out.push('\n');
                    }
                    index += 1;
                }
            }
            out.push(' ');
        } else if let Some(hashes) = raw_string_hashes(&chars, index) {
            index += 2 + hashes;
            out.push('"');
            while index < chars.len() {
                let closes = chars[index] == '"'
                    && (1..=hashes).all(|offset| chars.get(index + offset) == Some(&'#'));
                if closes {
                    index += 1 + hashes;
                    break;
                }
                if chars[index] == '\n' {
                    out.push('\n');
                }
                index += 1;
            }
            out.push('"');
        } else if current == '"' {
            index += 1;
            out.push('"');
            while index < chars.len() && chars[index] != '"' {
                if chars[index] == '\\' {
                    index += 1;
                }
                if chars.get(index) == Some(&'\n') {
                    out.push('\n');
                }
                index += 1;
            }
            index += 1;
            out.push('"');
        } else if current == '\'' && is_char_literal(&chars, index) {
            index += 1;
            while index < chars.len() && chars[index] != '\'' {
                if chars[index] == '\\' {
                    index += 1;
                }
                index += 1;
            }
            index += 1;
            out.push_str("' '");
        } else {
            out.push(current);
            index += 1;
        }
    }
    out
}

/// Returns the number of `#` marks when a raw string literal starts at `index`.
fn raw_string_hashes(chars: &[char], index: usize) -> Option<usize> {
    if chars[index] != 'r'
        || index
            .checked_sub(1)
            .is_some_and(|before| is_ident(chars[before]))
    {
        return None;
    }
    let hashes = chars[index + 1..]
        .iter()
        .take_while(|&&character| character == '#')
        .count();
    (chars.get(index + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// Tells a character literal such as `'a'` or `'\n'` from a lifetime `'a`.
fn is_char_literal(chars: &[char], index: usize) -> bool {
    match chars.get(index + 1) {
        Some('\\') => true,
        Some(_) => chars.get(index + 2) == Some(&'\''),
        None => false,
    }
}

fn is_ident(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Whether a `cfg` predicate is only true when compiling tests.
///
/// Only `test` and an `all(..)` with `test` as a direct argument qualify;
/// `any(test, ..)` and every other predicate also compile outside tests, so
/// the code they guard is scanned.
fn predicate_is_test_only(predicate: &str) -> bool {
    let predicate: String = predicate.chars().filter(|c| !c.is_whitespace()).collect();
    if predicate == "test" {
        return true;
    }
    let Some(arguments) = predicate
        .strip_prefix("all(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let mut depth = 0_usize;
    let mut argument = String::new();
    let mut arguments_list = Vec::new();
    for character in arguments.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments_list.push(std::mem::take(&mut argument));
                continue;
            }
            _ => {}
        }
        argument.push(character);
    }
    arguments_list.push(argument);
    arguments_list.iter().any(|argument| argument == "test")
}

/// Parses a `#[cfg(..)]` or `#![cfg(..)]` attribute at `index`.
///
/// Returns the index just past the attribute, whether it is an inner
/// attribute, and whether its predicate is test-only.
fn cfg_attribute_at(chars: &[char], index: usize) -> Option<(usize, bool, bool)> {
    if chars[index] != '#' {
        return None;
    }
    let mut cursor = index + 1;
    let inner = chars.get(cursor) == Some(&'!');
    if inner {
        cursor += 1;
    }
    if chars.get(cursor) != Some(&'[') {
        return None;
    }
    let close = matching_close(chars, cursor)?;
    let body: String = chars[cursor + 1..close].iter().collect();
    let body = body.trim();
    let predicate = body
        .strip_prefix("cfg")
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))?;
    Some((close + 1, inner, predicate_is_test_only(predicate)))
}

/// Returns the index of the bracket closing the one opened at `open`.
fn matching_close(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, &character) in chars[open..].iter().enumerate() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_whitespace(chars: &[char], mut index: usize) -> usize {
    while chars
        .get(index)
        .is_some_and(|character| character.is_whitespace())
    {
        index += 1;
    }
    index
}

fn starts_with_word(chars: &[char], index: usize, word: &str) -> bool {
    let end = index + word.chars().count();
    chars
        .get(index..end)
        .is_some_and(|slice| slice.iter().copied().eq(word.chars()))
        && !chars.get(end).copied().is_some_and(is_ident)
}

/// Returns the index just past the item, field or statement starting at
/// `index`, which a test-only attribute guards.
///
/// The guarded code ends at a `;` or `,` at its own nesting depth, or after a
/// closing `}` that is not followed by `else`, `.` or `?`. A closing bracket
/// that belongs to the enclosing code ends it without being consumed, so a
/// guarded struct field never swallows the lines that close its container.
fn end_of_guarded(chars: &[char], index: usize) -> usize {
    let mut cursor = skip_whitespace(chars, index);
    while chars.get(cursor) == Some(&'#') && chars.get(cursor + 1) == Some(&'[') {
        let Some(close) = matching_close(chars, cursor + 1) else {
            return chars.len();
        };
        cursor = skip_whitespace(chars, close + 1);
    }
    let mut depth = 0_usize;
    while cursor < chars.len() {
        match chars[cursor] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' => {
                if depth == 0 {
                    return cursor;
                }
                depth -= 1;
            }
            '}' => {
                if depth == 0 {
                    return cursor;
                }
                depth -= 1;
                if depth == 0 {
                    let after = skip_whitespace(chars, cursor + 1);
                    let continues = starts_with_word(chars, after, "else")
                        || matches!(chars.get(after), Some('.' | '?'));
                    if !continues {
                        return match chars.get(after) {
                            Some(';' | ',') => after + 1,
                            _ => cursor + 1,
                        };
                    }
                }
            }
            ';' | ',' if depth == 0 => return cursor + 1,
            _ => {}
        }
        cursor += 1;
    }
    cursor
}

/// Non-test code of one source file, plus the out-of-line test-only modules
/// it declares.
struct NonTestSource {
    code: String,
    test_only_modules: Vec<String>,
}

/// Returns the code that compiles outside tests, without comments, literal
/// contents or test-only items, fields and statements.
///
/// A file whose first attribute is `#![cfg(test)]` has no non-test code.
fn non_test_source(source: &str) -> NonTestSource {
    let stripped = strip_comments_and_literals(source);
    let chars: Vec<char> = stripped.chars().collect();
    let mut code = String::with_capacity(stripped.len());
    let mut test_only_modules = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if let Some((after_attribute, inner, test_only)) = cfg_attribute_at(&chars, index) {
            if test_only && inner {
                return NonTestSource {
                    code: String::new(),
                    test_only_modules,
                };
            }
            if test_only {
                let end = end_of_guarded(&chars, after_attribute);
                let guarded: String = chars[after_attribute..end].iter().collect();
                if let Some(module) = out_of_line_module(&guarded) {
                    test_only_modules.push(module);
                }
                code.extend(
                    chars[index..end]
                        .iter()
                        .filter(|&&character| character == '\n'),
                );
                index = end;
                continue;
            }
        }
        code.push(chars[index]);
        index += 1;
    }
    NonTestSource {
        code,
        test_only_modules,
    }
}

/// Returns the module name when `item` declares an out-of-line `mod name;`.
fn out_of_line_module(item: &str) -> Option<String> {
    let item = item.trim();
    let item = item.strip_prefix("pub(crate)").unwrap_or(item).trim_start();
    let item = item.strip_prefix("pub").unwrap_or(item).trim_start();
    let name = item.strip_prefix("mod")?.trim().strip_suffix(';')?.trim();
    (!name.is_empty() && name.chars().all(is_ident)).then(|| name.to_owned())
}

/// Returns the files an out-of-line `mod name;` in `file` may load.
fn module_files(file: &Path, name: &str) -> [PathBuf; 2] {
    let parent = file.parent().expect("should have a parent directory");
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("should have a UTF-8 file stem");
    let directory = if matches!(stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ]
}

/// Whether `symbol` occurs in `code` as a whole path, not inside a longer
/// identifier such as `handle_identify_v2`.
fn contains_symbol(code: &str, symbol: &str) -> bool {
    code.match_indices(symbol).any(|(start, _)| {
        let before = code[..start].chars().next_back();
        let after = code[start + symbol.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn adapter_uses() -> Vec<(String, &'static str)> {
    let crates = crates_dir();
    let mut uses = Vec::new();
    for adapter in ADAPTER_CRATES {
        let mut files = Vec::new();
        rust_files(&crates.join(adapter).join("src"), &mut files);
        let sources: Vec<_> = files
            .iter()
            .map(|file| {
                let source = fs::read_to_string(file).expect("should read adapter source file");
                (file, non_test_source(&source))
            })
            .collect();
        let test_only_files: Vec<PathBuf> = sources
            .iter()
            .flat_map(|(file, source)| {
                source
                    .test_only_modules
                    .iter()
                    .flat_map(|module| module_files(file, module))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (file, source) in &sources {
            if test_only_files.contains(file) {
                continue;
            }
            let relative = file
                .strip_prefix(&crates)
                .expect("should be under the crates directory")
                .to_string_lossy()
                .replace('\\', "/");
            for symbol in IDENTITY_ENTRY_POINTS {
                if contains_symbol(&source.code, symbol) {
                    uses.push((relative.clone(), *symbol));
                }
            }
        }
    }
    uses
}

#[test]
fn adapters_reach_identity_only_through_the_core_app() {
    let uses = adapter_uses();

    let unexpected: Vec<_> = uses
        .iter()
        .filter(|(file, symbol)| !PENDING_LIFECYCLE_MOVE.contains(&(file.as_str(), *symbol)))
        .collect();

    assert!(
        unexpected.is_empty(),
        "should reach identity through trusted_server_core::ec::app, found direct uses: {unexpected:?}"
    );
}

#[test]
fn pending_lifecycle_entries_are_still_in_use() {
    let uses = adapter_uses();

    let stale: Vec<_> = PENDING_LIFECYCLE_MOVE
        .iter()
        .filter(|(file, symbol)| {
            !uses
                .iter()
                .any(|(used_file, used)| used_file == file && used == symbol)
        })
        .collect();

    assert!(
        stale.is_empty(),
        "should remove pending entries that adapters no longer use: {stale:?}"
    );
}

fn code_of(source: &str) -> String {
    non_test_source(source).code
}

#[test]
fn non_test_source_drops_test_modules_and_items() {
    let source = "fn live() { handle_identify(); }\n#[cfg(test)]\nmod tests {\n    fn t() {\n        dispatch_pull_sync();\n    }\n}\n#[cfg(test)]\nuse crate::ec_finalize_response;\nfn after() { handle_batch_sync(); }\n";

    let code = code_of(source);

    assert!(
        contains_symbol(&code, "handle_identify"),
        "should keep live code"
    );
    assert!(
        !contains_symbol(&code, "dispatch_pull_sync"),
        "should drop test modules"
    );
    assert!(
        !contains_symbol(&code, "ec_finalize_response"),
        "should drop test-only use items"
    );
    assert!(
        contains_symbol(&code, "handle_batch_sync"),
        "should keep code after test items"
    );
}

#[test]
fn non_test_source_drops_only_a_test_only_field() {
    let source = "fn build() -> State {\n    Ok(Arc::new(State {\n        identity,\n        #[cfg(test)]\n        services_override: None,\n    }))\n}\nfn next() { handle_identify(); }\n";

    let code = code_of(source);

    assert!(
        !code.contains("services_override"),
        "should drop the test-only field"
    );
    assert!(
        code.contains("}))"),
        "should keep the lines that close the field's container"
    );
    assert!(
        contains_symbol(&code, "handle_identify"),
        "should keep code after the container"
    );
}

#[test]
fn non_test_source_drops_only_a_test_only_statement() {
    let source = "fn run() {\n    #[cfg(test)]\n    if probe { dispatch_pull_sync(); } else { other(); }\n    handle_identify();\n}\n";

    let code = code_of(source);

    assert!(
        !contains_symbol(&code, "dispatch_pull_sync"),
        "should drop the test-only statement"
    );
    assert!(
        !contains_symbol(&code, "other"),
        "should drop its else branch"
    );
    assert!(
        contains_symbol(&code, "handle_identify"),
        "should keep the next statement"
    );
}

#[test]
fn non_test_source_scans_code_that_also_compiles_outside_tests() {
    let source = "#[cfg(any(test, target_arch = \"wasm32\"))]\nfn shared() { handle_identify(); }\n#[cfg(not(test))]\nfn live() { handle_batch_sync(); }\n#[cfg(all(test, feature = \"x\"))]\nfn only_tests() { dispatch_pull_sync(); }\n";

    let code = code_of(source);

    assert!(
        contains_symbol(&code, "handle_identify"),
        "should scan cfg(any(test, ..)) code"
    );
    assert!(
        contains_symbol(&code, "handle_batch_sync"),
        "should scan cfg(not(test)) code"
    );
    assert!(
        !contains_symbol(&code, "dispatch_pull_sync"),
        "should drop cfg(all(test, ..)) code"
    );
}

#[test]
fn non_test_source_drops_comments_but_not_code_after_strings() {
    let source = "// handle_identify\n/* handle_batch_sync /* nested */ dispatch_pull_sync */\nfn f() { let url = \"https://example.com/x\"; ec_finalize_response(); }\nfn g() { log(\"generate_if_needed\"); let c = '\"'; let r = r#\"// x\"#; answer_identify_locally(); }\nfn h<'a>(value: &'a str) { answer_batch_sync_locally(value); }\n";

    let code = code_of(source);

    assert!(
        !contains_symbol(&code, "handle_identify"),
        "should drop line comments"
    );
    assert!(
        !contains_symbol(&code, "handle_batch_sync"),
        "should drop block comments"
    );
    assert!(
        !contains_symbol(&code, "dispatch_pull_sync"),
        "should drop nested block comments"
    );
    assert!(
        contains_symbol(&code, "ec_finalize_response"),
        "should keep code after a string holding //"
    );
    assert!(
        !contains_symbol(&code, "generate_if_needed"),
        "should ignore string contents"
    );
    assert!(
        contains_symbol(&code, "answer_identify_locally"),
        "should keep code after char and raw string literals"
    );
    assert!(
        contains_symbol(&code, "answer_batch_sync_locally"),
        "should treat lifetimes as code"
    );
}

#[test]
fn non_test_source_skips_files_gated_on_test_and_records_test_modules() {
    let gated = non_test_source("#![cfg(test)]\nfn f() { handle_identify(); }\n");
    let declaring = non_test_source("#[cfg(test)]\nmod tests;\nmod live;\n");

    assert!(
        gated.code.is_empty(),
        "should treat an inner cfg(test) file as test-only"
    );
    assert_eq!(
        declaring.test_only_modules,
        vec!["tests".to_owned()],
        "should record only the test-only out-of-line module"
    );
}

#[test]
fn contains_symbol_matches_whole_paths_only() {
    assert!(
        contains_symbol("x = handle_identify(req);", "handle_identify"),
        "should match a call"
    );
    assert!(
        !contains_symbol("handle_identify_v2(req);", "handle_identify"),
        "should not match a longer identifier"
    );
    assert!(
        !contains_symbol("prehandle_identify(req);", "handle_identify"),
        "should not match a suffix"
    );
    assert!(
        contains_symbol(
            "PartnerRegistry::from_config(x)",
            "PartnerRegistry::from_config"
        ),
        "should match a path"
    );
}
