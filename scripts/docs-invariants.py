#!/usr/bin/env python3
"""Cross-document invariant sweep for the #1009 document set.

`npm run format` catches formatting. `npm run build` catches dead links. Neither
catches a claim corrected in one document and left standing in another, which is
how every review round on this branch has found real defects.

DESIGN NOTES — a previous version of this checker reported 8/8 green on
documents that still contained the exact contradictions it claimed to check.
Three things caused that, and each is addressed here:

1. It matched literal phrases ("two seams") that the stale text did not use
   ("two existing injection seams"). Patterns are now semantic and tolerant.
2. It matched line by line, so any phrase wrapped across a line break was
   invisible. Text is now whitespace-normalized per file before matching.
3. It had no way to know it had stopped working. Every check now carries
   `must_flag` fixtures — strings that MUST trip it — and the script fails if
   any fixture is not caught. A check that cannot fail is treated as broken.
4. It let unrelated nearby correction language excuse a violation. Generic
   allow-windows are gone; the one qualified check must span the exact API
   occurrence it excuses.

The four promised input documents and both fixture directions are mandatory.
Missing inputs, unreadable inputs, or an empty fixture side make the checker
itself fail with exit 2 before it reports document results.

A false positive costs a minute. A false green costs a merge.
"""
import re
import sys
import glob
import os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")

REQUIRED_TARGETS = frozenset(
    {
        "docs/superpowers/plans/2026-08-08-1009-measurement-and-stage-0.md",
        "docs/superpowers/plans/2026-08-08-1009-measurement-findings.md",
        "docs/superpowers/plans/2026-08-10-1009-esi-validation-spike.md",
        "docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md",
    }
)


def targets():
    return sorted(
        glob.glob(os.path.join(ROOT, "docs/superpowers/specs/2026-08-08-esi-*.md"))
        + glob.glob(os.path.join(ROOT, "docs/superpowers/plans/2026-08-*1009*.md"))
    )


def target_errors(paths):
    """Return setup errors when any document this gate promises to scan is absent."""
    present = {
        os.path.relpath(path, ROOT).replace(os.sep, "/")
        for path in paths
    }
    return [
        f"  missing required document: {path}"
        for path in sorted(REQUIRED_TARGETS - present)
    ]


def load_documents(paths):
    documents = {}
    errors = []
    for path in paths:
        try:
            with open(path, encoding="utf-8") as source:
                documents[path] = normalize(source.read())
        except (OSError, UnicodeError) as error:
            errors.append(f"  cannot read {os.path.relpath(path, ROOT)}: {error}")
    return documents, errors


def normalize(text):
    """Collapse wraps while removing Markdown blockquote continuation markers."""
    text = re.sub(r"(?m)^\s*>\s?", "", text)
    text = re.sub(r"\s*\n\s*", " ", text)
    text = re.sub(r"[ \t]{2,}", " ", text)
    return text


class Check:
    def __init__(self, name, bad, must_flag=(), must_pass=()):
        self.name = name
        self.bad = re.compile(bad, re.I)
        self.must_flag = must_flag   # strings this check MUST catch
        self.must_pass = must_pass   # corrected strings it must NOT catch

    def hits(self, normalized):
        out = []
        for m in self.bad.finditer(normalized):
            out.append(normalized[max(0, m.start() - 60):m.end() + 60].strip())
        return out


class QualifiedOccurrenceCheck(Check):
    """Flag each occurrence unless its own local clause states the required relation."""

    def __init__(self, name, occurrence, qualified, must_flag=(), must_pass=()):
        super().__init__(name, occurrence, must_flag=must_flag, must_pass=must_pass)
        self.qualified = re.compile(qualified, re.I)

    def hits(self, normalized):
        out = []
        qualified_spans = [match.span() for match in self.qualified.finditer(normalized)]
        for m in self.bad.finditer(normalized):
            if any(lo <= m.start() and m.end() <= hi for lo, hi in qualified_spans):
                continue
            out.append(normalized[max(0, m.start() - 60):m.end() + 60].strip())
        return out


CHECKS = [
    Check(
        "Stage 0 gates on FINAL PASS, not a bare Vary check",
        # `[*_ ]*` absorbs markdown emphasis; `gated on a curl` is its own shape
        # because the sentence boundary defeats a proximity match to "Stage 0".
        r"gates?[*_ ]+stage[*_ ]*0"
        r"(?!\s*only\s+(?:once|after|when)\s+(?:an?\s+)?[`*_]*final\s+pass[`*_]*"
        r"(?:\s+is)?\s+(?:recorded|obtained|achieved)\b)"
        r"|verdict[:*_ ]+pass[*_ ]*/[*_ ]*fail"
        r"|gated\s+on\s+a\s+.?curl",
        must_flag=[
            "inspect the `Vary` response header. **Gates Stage 0**, the only build item",
            "**Verdict:** PASS / FAIL",
            "Stage 0 next, shipped as the operator flag. Gated on a `curl`, reverses an origin-load cost",
            "Inspect Vary. **Gates Stage 0** immediately. A separate sentence says FINAL PASS is required.",
            "Gates Stage 0 immediately; FINAL PASS gates deployment later.",
            "The Vary result gates Stage 0 only on a successful curl, while FINAL PASS gates production rollout.",
        ],
        must_pass=[
            "Gates Stage 0 only once a FINAL PASS is recorded",
            "Gates Stage 0 only after a FINAL PASS is recorded",
            "**Verdict:** FINAL PASS / PROVISIONAL PASS / FAIL",
        ],
    ),
    Check(
        "Rollback is not described as config-only",
        r"rolls?\s+back\s+with\s+a\s+config\s+push"
        r"|rollback\s+is\s+another\s+config\s+push\s+rather",
        must_flag=[
            "reverses an origin-load cost, and rolls back with a config push. Closer to a defect fix",
            "This rolls back with a config push, explicitly without a purge or TTL wait.",
        ],
        must_pass=[
            "the read path reverts with a config push, but full rollback also needs a C1 purge path",
        ],
    ),
    Check(
        "Template has one marker, not two seams",
        r"\b(?:emit|emits|use|uses|place|places|insert|inserts)\b[^.]{0,100}"
        r"(?:two|both)\s+(?:existing\s+)?(?:injection\s+)?seams"
        r"|markers?\s+at\s+(?:the\s+)?two\s+(?:existing\s+)?(?:injection\s+)?seams"
        r"|adSlots[^.]{0,60}stays?\s+in\s+the\s+template",
        must_flag=[
            "The transform emits `esi:include` markers at the two existing injection seams instead",
            "emit markers at the two seams",
            "Emit markers at the two existing injection seams. Correction: not two; use one marker.",
        ],
        must_pass=[
            "one unconditional marker at the body-close seam. Not two: the head seam is not a template hole",
            "The earlier draft used two injection seams; that statement was wrong.",
        ],
    ),
    Check(
        "Headers finalize before assembly, in prose and diagrams",
        r"assemble\s*(→|->)\s*finaliz"
        r"|finaliz\w*[^.]{0,40}(runs?|still run)[^.]{0,20}after\s+assembly",
        must_flag=[
            "`origin → lol_html transform → fastly::cache::core → assemble → finalize`",
            "esi assemble  →  finalize  →  client",
            "Cookie and privacy finalization still run after assembly",
        ],
        must_pass=[
            "fastly::cache::core → finalize headers → stream assembly",
            "Cookie and privacy finalization ran BEFORE assembly, not after",
        ],
    ),
    Check(
        "No nonexistent Core Cache APIs",
        r"\b[A-Za-z_]\w*\.to_body\s*\(\s*\)",
        must_flag=[
            "let template = found.to_body();",
            "let template = found.to_body(); // there is no fallback",
        ],
        must_pass=["found.to_stream()?  // fallible; there is no `to_body()`"],
    ),
    Check(
        "A2/A3 not decided on root TTFB",
        r"A3\s+beats\s+A2\s+on\s+TTFB",
        must_flag=["2. A3 beats A2 on TTFB by a margin the reviewers ratify"],
        must_pass=["A3 beats A2 on bids-ready time, adInit fire time, and first attributed paint"],
    ),
    Check(
        "ESI not described as impossible",
        r"\bESI\s+(?:is|remains)\s+(?:not\s+viable|structurally\s+(?:blocked|impossible))",
        must_flag=[
            "Answers #1009: ESI is not viable here, for a structural reason",
            "ESI is not viable here. An earlier revision discussed a different problem.",
        ],
        must_pass=["The first revision concluded that ESI was structurally blocked. Both claims are false"],
    ),
    QualifiedOccurrenceCheck(
        "C1 and C2 purge surfaces not conflated",
        r"InsertBuilder::surrogate_keys",
        r"InsertBuilder::surrogate_keys(?:\(\[\.\.\.\]\))?"
        r"(?:(?!InsertBuilder::surrogate_keys|[.]|\bnot\b|\bnever\b|\bno\s+longer\b).){0,60}"
        r"(?:\(\s*Core\s+Cache\s*\)|(?:is|belongs\s+to)"
        r"(?![^.]{0,20}\b(?:not|never)\b|[^.]{0,20}\bno\s+longer\b)"
        r"[^.]{0,60}Core\s+Cache)",
        must_flag=[
            "Purge is available in-process, with keys attached at insert via `InsertBuilder::surrogate_keys`. Rollback is therefore flip then purge.",
            "Attach C1 keys via `InsertBuilder::surrogate_keys`. C2 is described below.",
            "C1 uses `InsertBuilder::surrogate_keys`. Separately, `InsertBuilder::surrogate_keys` is the Core Cache API for C2.",
            "C1 uses `InsertBuilder::surrogate_keys`, but `InsertBuilder::surrogate_keys` is the Core Cache API for C2.",
            "`InsertBuilder::surrogate_keys` is the Core Cache API for C2, but C1 uses `InsertBuilder::surrogate_keys`.",
            "C1 uses `InsertBuilder::surrogate_keys`; it is not the Core Cache API.",
            "C1 uses `InsertBuilder::surrogate_keys`; it no longer belongs to the Core Cache API.",
            "This is not C2; `InsertBuilder::surrogate_keys` powers C1.",
        ],
        must_pass=[
            "`InsertBuilder::surrogate_keys` is the Core Cache API and keys C2. It does **not** key C1.",
        ],
    ),
]


def self_test():
    """A check that cannot fail is broken. Prove each one still fires."""
    broken = []
    for c in CHECKS:
        if not c.must_flag:
            broken.append(f"  {c.name!r}: has no must_flag fixtures")
        if not c.must_pass:
            broken.append(f"  {c.name!r}: has no must_pass fixtures")
        for bad in c.must_flag:
            if not c.hits(normalize(bad)):
                broken.append(f"  {c.name!r}: FAILED to flag known-bad text:\n    {bad[:110]}")
        for good in c.must_pass:
            if c.hits(normalize(good)):
                broken.append(f"  {c.name!r}: wrongly flagged corrected text:\n    {good[:110]}")

    if not target_errors([]):
        broken.append("  target guard accepted an empty document set")
    synthetic_targets = [os.path.join(ROOT, path) for path in REQUIRED_TARGETS]
    if errors := target_errors(synthetic_targets):
        broken.append(f"  target guard rejected its required manifest: {errors}")
    return broken


def report_checker_errors(errors):
    print("CHECKER IS BROKEN — its own fixtures or document setup do not pass.")
    print("A green run from this state would be meaningless.\n")
    print("\n".join(errors))


def main():
    paths = targets()
    broken = self_test() + target_errors(paths)
    if broken:
        report_checker_errors(broken)
        return 2

    documents, read_errors = load_documents(paths)
    if read_errors:
        report_checker_errors(read_errors)
        return 2

    fail = False
    for c in CHECKS:
        found = []
        for path, document in documents.items():
            for h in c.hits(document):
                found.append(f"  {os.path.relpath(path, ROOT)}: …{h}…")
        if found:
            fail = True
            print(f"\n[FAIL] {c.name}")
            print("\n".join(found))
        else:
            print(f"[ok]   {c.name}")

    print()
    print("Contradictions found — do not push." if fail else
          f"All {len(CHECKS)} invariants hold (self-test passed).")
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
