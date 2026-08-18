# PR 823 Review Resolution Design

## Goal

Resolve the actionable findings in review `4958563121` on PR 823 without
unrelated refactoring, verify the complete branch, publish the fixes, and reply
to every inline review thread with concrete resolution evidence.

## Scope

The implementation covers all 28 inline threads and all actionable items in the
review summary. The summary's explicitly out-of-scope pre-existing
partially-invalid `page_patterns` behavior is not expanded into this PR unless a
fix is required by another in-scope change. The PR description's stale legacy
alias sentence is corrected after the branch changes are published.

Each reviewer suggestion is verified against the current code. A suggestion is
implemented when it is correct for this repository. Where repository evidence
contradicts a suggestion, the implementation retains the correct behavior and
the review response explains the evidence.

## Design Principles

- Preserve operator-authored configuration, comments, ordering, and unrelated
  sections byte-for-byte wherever possible.
- Never print secrets or whole effective configuration documents as diagnostic
  output.
- Never turn uncertain crawl evidence into a runnable fabricated ad-unit path.
- Treat browser navigation as a session, not a sequence of isolated launches.
- Keep `generate`, `verify`, static CLI commands, and runtime matching on shared
  domain rules instead of parallel reimplementations.
- Bound all page-controlled data and browser operations.
- Use test-first changes for behavior corrections and minimal annotations for
  code-quality-only corrections.

## Component Design

### 1. Configuration integrity and command output

`slot_toml` will replace the line-oriented slot-boundary heuristic with a
TOML-aware edit strategy. The resulting document must preserve every top-level
item outside the managed creative-opportunity fields and preserve comments
adjacent to or between operator sections. Non-contiguous slot declarations,
multiline values, arrays whose continuation lines begin with `[`, trailing
comments, CRLF input, and inline-slot conversion receive regression coverage.
The updater will reject a candidate if preservation cannot be proven.

Generation will re-read the source config immediately before the atomic write
and refuse to overwrite a concurrently edited file. `--dry-run` will emit only
the managed creative-opportunities change, never the complete config. Notes and
rollback warnings go to stderr so machine-readable stdout remains clean. Tests
will prove that dry-run leaves the source file byte-identical and does not expose
unrelated secret-bearing keys.

Merge behavior remains add-only for operator-authored data: existing templated
unit paths are retained, newly observed formats are unioned, and multiple
discovered placements absorbed by one broad configured div prefix produce an
operator note.

### 2. Crawl evidence and inference

Inference will preserve evidence instead of silently collapsing it:

- Non-ASCII shared-prefix computation uses UTF-8 byte boundaries.
- Same-page normalization collisions retain distinct raw placements and emit a
  diagnostic rather than silently dropping formats. Numeric-only stable tokens
  are not classified as hexadecimal hash noise.
- Multi-slot SRA request fallbacks are ignored when `dids` names more than one
  slot.
- A page is considered empty only when no audited profile found slots there.
- Fragment detection requires stronger evidence: a useful shared prefix, or at
  least three disjoint fragments. Ambiguous two-slot groups are retained with a
  note.
- Locale landing paths are emitted literally when they are shorter than the
  inferred section depth, and literal path segments are escaped before being
  interpolated into globs.
- Refused template decisions are omitted from generated slots and surfaced with
  their reasons. The documentation and tests will consistently describe these
  cases as refusal, not literal fallback.
- The redundant witness rule is removed or made independently meaningful. The
  actual crawler will support the section depth that inference can produce;
  locale-prefixed behavior will not exist only in hand-built evidence tests.
- Dropped-section diagnostics are capped, percent-encoded paths are normalized
  before filtering, and page-like extensions are classified consistently.

The root page and section pages for a device profile are collected in one
browser session. Page analysis that parses full HTML is moved off the
current-thread CDP event pump. Each page/tab is closed on every success and
error path.

### 3. Shared browser behavior

The browser collectors will share executable discovery and launch/session
configuration. Browser options exposed to operators will have one meaning in
`page`, `verify`, and `generate`: Chrome override, settling, headful/headless
mode, device profile/viewport, proxy, consent assumption, cookies, and TLS
policy.

`verify` will reuse one browser/runtime/profile across its URLs so clearance and
session state survive. The generic/legacy generator will default to the same
consent assumption as ad-template generation and expose the opt-out rather than
depending on `derive(Default)`.

Cookie parameters are explicitly host-only with `Path=/`. A same-host
`http`-to-`https` upgrade is accepted with a redirect note; host changes,
downgrades, and unexpected port changes remain cross-origin refusals. Failure to
read or parse the final browser URL fails closed instead of substituting the
requested URL.

Every post-navigation evaluation is time-bounded. The collector enlarges the
resource timing buffer before navigation, waits for an interactive or complete
document before accruing quiet time, honors sub-poll quiet windows, validates
`quiet <= max`, and reports saturation. Navigation load-event timeout is a
warning after a successful `goto`; it does not discard readable page evidence.
Evidence payload bytes and captured string lengths are capped before expensive
decode/allocation.

Init-script and page-evaluation failures become explicit warnings or errors
rather than empty evidence. Promise-returning sitemap evaluation awaits its
result. Main-frame-only collection is disclosed when frames are skipped.

The injected collector will be behavior-preserving: size pairs enforce the
`u32` range, the `googletag` setter is total, the unused non-variadic `cmd.push`
wrapper is removed, wrapping markers are closure-local/non-enumerable, and
page-derived warning text is terminal-safe.

### 4. Runtime and static-command parity

Expected-slot projection uses the runtime's renderability rule. Slots the
runtime omits for a path do not count as matched verification slots; diagnostics
state that the runtime omits the slot on that path rather than claiming the
whole config is rejected.

Configured media type remains a typed `MediaType` through comparison and is
rendered to a string only at the output boundary. Slots that the phase-one
checker cannot confirm (video/native-only and out-of-page) are represented as
unconfirmable and do not fail `--strict`; genuinely partial or missing
confirmable slots still fail. Slot phase is absent when no evidence exists.
The server-side APS compatibility field no longer creates unconditional
client-side `fetchBids` warnings.

Collector warnings are included in page results. Human output includes the
runtime expectation, gate summary, matched count, extra evidence, and warnings
already present in JSON. Output escaping covers Unicode bidi controls and all
config-derived strings.

`explain` reports exactly the shared runtime gate result. Provider configuration
is a separate advisory. The unsupported `--edgezero-enabled` model and stale
legacy-fallback claim are removed because no runtime condition backs them.
Gate diagnostics consume the shared gate result instead of rebuilding lists by
hand. The hot runtime gate avoids heap allocation, the seven-boolean wrapper is
removed, and the consent tri-state is documented and exhaustively tested.

`compile_page_pattern` becomes crate-private and a public validation-only API is
used by the CLI. `lint` explicitly reports every configured page pattern the
runtime would drop, while the broader pre-existing runtime acceptance policy
remains out of scope. Specific compile failures are retained in logs. HTTP
methods use `http::Method` parsing so CLI semantics match the runtime.

Full URLs and bare path inputs pass through the same URL normalization rules:
percent-encoding, dot-segment resolution, query/fragment removal, and leading
slash behavior must be identical. Scheme detection is anchored to the path
portion before `?`, so an absolute URL inside a query value does not cause a
bare path to be parsed as a full URL.

### 5. CLI contracts, documentation, and CI

Clap owns argument validation: URL parsing happens at the value parser, the
audit namespace uses help-on-missing-subcommand, `check` uses an argument group
and conflicts, and settle bounds are rejected during parsing. Parser tests cover
the visible command shapes and legacy restrictions.

CI-oriented assertion failures exit 1; tool/configuration/navigation failures
exit 2. Assertion text is written directly and cannot disappear behind a log
filter. The guide documents all four `ts config ad-templates` commands, all
flags, shared config-loading flags, browser flags, consent/profile behavior,
dry-run output, and exit codes.

Browser fixture CI either installs/resolves Chrome and requires the tests to
execute, or explicitly opts into a mode that fails when Chrome is unavailable;
it may not report success after silently skipping every browser assertion.

All real-looking customer identifiers and names introduced by this PR are
replaced with fictional values in tests, comments, and documentation. Stale
module-level lint suppressions, inaccurate docs, assertion messages, enum
ordering, dead query matching, and orphaned comments are corrected without
unrelated cleanup.

## Inline Review Traceability

| Thread                     | Resolution area                                                    |
| -------------------------- | ------------------------------------------------------------------ |
| `3802056460`, `3802056470` | TOML-aware splice and comment/value preservation                   |
| `3802056474`               | Secret-safe dry-run and stderr diagnostics                         |
| `3802056481`               | Omit and explain refused slots                                     |
| `3802056488`               | UTF-8-safe div prefix calculation                                  |
| `3802056494`               | Same-page normalized-div collisions                                |
| `3802056497`               | Locale landing-page patterns                                       |
| `3802056502`               | Multi-profile empty-page accounting                                |
| `3802056508`               | Close every browser tab                                            |
| `3802056513`               | Enforce JavaScript-to-Rust `u32` bounds                            |
| `3802056521`, `3802056529` | Total GPT hook and removal of behavior-changing `cmd.push` wrapper |
| `3802056539`               | Shared faithful browser launch configuration                       |
| `3802056549`, `3802056555` | Correct settling and load-timeout handling                         |
| `3802056559`               | Preserve injected collector warnings                               |
| `3802056564`, `3802056571` | Runtime renderability parity and accurate diagnostics              |
| `3802056580`, `3802056584` | Unconfirmable status and removal of false APS warning              |
| `3802056586`               | Identical URL and bare-path normalization                          |
| `3802056593`               | Fictional committed examples                                       |
| `3802056599`               | Browser fixture CI must execute or fail loudly                     |
| `3802056605`               | Add-only merge of formats with broad-prefix diagnostics            |
| `3802056614`               | Consent parity for generic and legacy generation                   |
| `3802056623`               | Refusal behavior, tests, and documentation agree                   |
| `3802056628`               | Safe same-host HTTP-to-HTTPS redirect handling                     |
| `3802056638`               | Remove ungrounded EdgeZero fallback model                          |

## Error Handling and Compatibility

All new Rust fallible paths use the repository's existing `CliResult` /
`error-stack` conventions. Browser failures identify the operation and URL but
do not include cookies, configuration values, or page payloads. Best-effort
cleanup must not replace an earlier collection error.

JSON compatibility is preserved where possible. New distinctions are additive
or correct semantically invalid fields: unconfirmable status is explicit, and
phase may be omitted when there was no evidence. Documentation is updated with
the exact wire behavior.

## Verification Strategy

Each behavioral issue follows red-green-refactor:

1. Add the smallest unit, parser, orchestration, or fixture test reproducing the
   review finding.
2. Run the narrow test and confirm the expected failure.
3. Implement the minimal correction.
4. Re-run the narrow test and the affected crate suite.

Final verification runs the repository-required commands relevant to the
changed surface: CLI tests through `scripts/test-cli.sh`, target-matched Rust
tests, JS tests when the collector script changes, `cargo fmt --all -- --check`,
all target-matched clippy aliases, documentation formatting, and browser fixture
tests with an available Chrome. Any environment-dependent test that cannot run
is reported explicitly and is not described as passing.

## Review Replies and Publication

Changes are grouped into reviewable commits by component, then pushed to the PR
branch after final verification. Each inline reply is posted in its existing
thread and states the concrete change, relevant test, or evidence-backed reason
for retaining behavior. Replies avoid generic acknowledgements. Threads are not
replied to as fixed until the corresponding commit is visible on GitHub.
