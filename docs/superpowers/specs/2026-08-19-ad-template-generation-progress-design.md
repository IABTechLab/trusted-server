# Ad-template generation progress design

## Problem

`ts audit ad-templates generate` audits up to the configured page budget for
each selected device profile (17 pages by default). Navigation and page settling
are intentionally bounded but can still take tens of seconds per page. The
browser collector buffers page results until the browser session closes, so the
command currently emits no output during most of that work and appears stuck.

## Design

Emit line-oriented progress on stderr while collection is running. Progress
must identify the device profile, current page, known total, and safe page
location. It must also identify non-page phases where a noticeable pause can
occur: launching the browser, planning the crawl after the root page, and
finalizing the browser session.

Progress is an explicit collector callback rather than direct terminal output
inside the browser implementation. This keeps output policy in the command
layer, makes the behavior testable with in-memory writers, and lets non-browser
collectors preserve the same contract. Each line is flushed immediately.

The first profile's root navigation has no final total because follow-up pages
are planned from the rendered root. It is reported as `1/?`; once planning
finishes, subsequent pages use a stable `current/total` count. Later profiles
receive the complete target list and report the root as `1/total`. Totals include
the root, and every attempted page advances the current count even if collection
fails.

Progress never prints a full URL. It renders only the origin-free path, omitting
userinfo, query, and fragment data, then applies the CLI's existing terminal-text
sanitizer. An empty path is rendered as `/`.

Stdout remains reserved for the generated diff or success summary. This keeps
`--dry-run` and shell redirection stable. Progress is intentionally plain text,
not an animated spinner, so it remains useful in logs and does not add a terminal
UI dependency.

## Error handling

Failure to write or flush progress is returned as a normal CLI output error. A
callback failure during a browser session stops further collection but does not
skip finalization, browser close, or process wait. An earlier collection or
planning error takes precedence over a later progress error; either takes
precedence over teardown errors. Close and wait are still attempted
independently. No cookie values, URL credentials, query values, fragments, or
browser credentials are included in progress.

## Tests

Unit tests will verify that progress is emitted before collection completes,
contains the specified profile-aware page counts, keeps stdout unchanged,
redacts URL credentials/query/fragment data, sanitizes paths, and reports
finalization. Writer tests will cover write failure, flush failure, and explicit
flush invocation. Collector tests will verify teardown still runs after progress
failure and that collection/planning errors, progress errors, and teardown errors
retain the stated precedence. The existing CLI and Chrome-backed suites will
verify the collector behavior and browser lifecycle remain intact.
