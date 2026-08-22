# PR 823 Round-5 Review Resolution

## Goal

Resolve review `4989897698` on PR 823 without weakening the generator's safety
rules, silently changing existing CLI defaults, or expanding the change beyond
the audit CLI and its documentation.

## Browser and CLI Compatibility

The hidden `ts audit <url>` compatibility form keeps accepting the same browser
flags as `ts audit generate <url>`, but those flags must remain hidden and must
require the legacy URL positional. A dedicated `LegacyBrowserOpts` mirrors the
seven generation browser fields and converts into `GenerateBrowserOpts` when the
legacy command is dispatched. Consequently, flags placed before a real audit
subcommand are rejected instead of parsed and ignored.

Generation retains its established 750 ms quiet period and 12-second maximum
settle wait. Generation defaults have one source of truth shared by clap,
`GenerateBrowserOpts::default`, and `BrowserAuditCollector::default`; applying
parsed options must not silently shorten the collector's maximum. The generic
page/verification collector keeps its existing independent 10-second default.

Redirect notes show the origin and path for both requested and final URLs. This
makes scheme and host changes visible without exposing URL userinfo, queries, or
fragments.

## Root-Less Template Safety

Template inference records which slot stems borrowed the config-level
`section_root` because those slots were never witnessed on a path without the
configured section segment. Such a template is safe only while its page patterns
are derived from the paths where the slot was observed.

Operator-supplied `--page-pattern` values replace those derived patterns for
every slot. If inference contains any borrowed-root slot and explicit patterns
were supplied, generation fails before rendering or writing a candidate config.
The error identifies the affected slots, explains that explicit patterns cannot
prove the borrowed-root invariant, and directs the operator to remove
`--page-pattern`. Failing the command is preferable to silently omitting real
inventory or attempting an unsound glob intersection.

When no config-level section policy can be inferred because every otherwise
templatable slot lacks a root witness, each affected slot's refusal reason names
that crawl gap rather than claiming that its paths failed to generalize.

## Merge Policy

An explicitly configured `section_segment` is operator intent even when
`section_root` is currently unset. If preserved `{section}` slots exist and an
inferred policy would change that configured segment, merge fails and requires
`--replace` for the migration. If the configured segment matches, or is unset,
the inferred `section_root` may be adopted so the previously incomplete config
becomes loadable.

## Diagnostics and Early Validation

Warnings produced while folding a collected page include the device-profile
label as well as the path. Identical warnings from desktop and mobile therefore
remain distinguishable. The consent-stub warning remains a single unscoped
run-level note, and site-wide discovery warnings remain deduplicated.

The existing config is parsed as TOML before Chrome starts. A whole-document
syntax error is returned immediately; a valid document with settings unknown to
the CLI still permits extraction of `[creative_opportunities]`; and a present
but unreadable creative section remains an error.

The volatile div-id token recognizer requires at least ten leading digits plus
an alphanumeric suffix. This continues to recognize timestamp-like generated
tokens while preventing an eight-digit calendar date followed by a stable
letter from causing a single-observation family refusal.

## Consistency Corrections

Tests pin the Rust evidence cap to the embedded JavaScript collector constant.
The terminal-escaping test claims only controls it can actually inject; URL's
own percent-encoding is covered by an exact final-URL assertion rather than
presented as evidence for terminal escaping. Existing code escaping the final
URL remains as defense in depth.

The affected guide, prior volatile-collision spec and plan, documentation
comments, `expect` message, and method spacing are corrected to describe the
implemented behavior exactly. The root-less templating behavior and this review
resolution are documented by this design and its paired implementation plan.

## Testing and Delivery

Every behavioral correction starts with a focused regression test that fails on
the current branch. Tests cover hidden legacy flags, the 12-second generation
default, complete redirect notes, borrowed-root rejection with explicit
patterns, configured-segment preservation, profile-specific warnings,
whole-document TOML failure, the evidence-cap invariant, and the calendar-date
token control.

After focused tests pass, verification runs the host-target CLI suite and
audit/generate tests, CLI clippy with warnings denied, Rust formatting, docs
formatting, and `git diff --check`. No GitHub replies or push are part of this
change unless separately requested.
