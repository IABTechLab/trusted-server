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

A false positive costs a minute. A false green costs a merge.
"""
import re
import sys
import glob
import os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def targets():
    return sorted(
        glob.glob(os.path.join(ROOT, "docs/superpowers/specs/2026-08-08-esi-*.md"))
        + glob.glob(os.path.join(ROOT, "docs/superpowers/plans/2026-08-*1009*.md"))
    )


def normalize(text):
    """Collapse whitespace so wrapped phrases match, and strip table padding."""
    text = re.sub(r"\s*\n\s*", " ", text)
    text = re.sub(r"[ \t]{2,}", " ", text)
    return text


class Check:
    def __init__(self, name, bad, allow=None, must_flag=(), must_pass=()):
        self.name = name
        self.bad = re.compile(bad, re.I)
        self.allow = re.compile(allow, re.I) if allow else None
        self.must_flag = must_flag   # strings this check MUST catch
        self.must_pass = must_pass   # corrected strings it must NOT catch

    def hits(self, normalized):
        out = []
        for m in self.bad.finditer(normalized):
            lo, hi = max(0, m.start() - 240), min(len(normalized), m.end() + 240)
            if self.allow and self.allow.search(normalized[lo:hi]):
                continue
            out.append(normalized[max(0, m.start() - 60):m.end() + 60].strip())
        return out


CHECKS = [
    Check(
        "Stage 0 gates on FINAL PASS, not a bare Vary check",
        # `[*_ ]*` absorbs markdown emphasis; `gated on a curl` is its own shape
        # because the sentence boundary defeats a proximity match to "Stage 0".
        r"gates?[*_ ]+stage[*_ ]*0(?![^.]{0,140}final pass)"
        r"|verdict[:*_ ]+pass[*_ ]*/[*_ ]*fail"
        r"|gated\s+on\s+a\s+.?curl",
        allow=r"final pass|provisional pass",
        must_flag=[
            "inspect the `Vary` response header. **Gates Stage 0**, the only build item",
            "**Verdict:** PASS / FAIL",
            "Stage 0 next, shipped as the operator flag. Gated on a `curl`, reverses an origin-load cost",
        ],
        must_pass=[
            "Gates Stage 0 only once a FINAL PASS is recorded",
            "**Verdict:** FINAL PASS / PROVISIONAL PASS / FAIL",
        ],
    ),
    Check(
        "Rollback is not described as config-only",
        r"rolls?\s+back\s+with\s+a\s+config\s+push(?![^.]{0,160}(purge|ttl))"
        r"|rollback\s+is\s+another\s+config\s+push\s+rather",
        must_flag=[
            "reverses an origin-load cost, and rolls back with a config push. Closer to a defect fix",
        ],
        must_pass=[
            "the read path reverts with a config push, but full rollback also needs a C1 purge path",
        ],
    ),
    Check(
        "Template has one marker, not two seams",
        r"(two|both)\s+(existing\s+)?(injection\s+)?seams"
        r"|markers?\s+at\s+the\s+two\s+seams"
        r"|adSlots[^.]{0,60}stays?\s+in\s+the\s+template",
        allow=r"not two|the head seam is not|NOT a hole",
        must_flag=[
            "The transform emits `esi:include` markers at the two existing injection seams instead",
            "emit markers at the two seams",
        ],
        must_pass=[
            "one unconditional marker at the body-close seam. Not two: the head seam is not a template hole",
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
        r"(?<!into_)\bto_body\s*\(\s*\)",
        allow=r"no `to_body|there is no",
        must_flag=["let template = found.to_body();"],
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
        r"ESI\s+is\s+not\s+viable|structurally\s+(blocked|impossible)",
        allow=r"concluded that ESI was|was wrong|original|earlier revision",
        must_flag=["Answers #1009: ESI is not viable here, for a structural reason"],
        must_pass=["The first revision concluded that ESI was structurally blocked. Both claims are false"],
    ),
    Check(
        "C1 and C2 purge surfaces not conflated",
        # Plain match plus a context-window allowlist: a negative lookahead is
        # useless here because `surrogate_keys([...])` is full of `.` characters.
        r"InsertBuilder::surrogate_keys",
        allow=r"core cache|\bC2\b|template cache|does \*\*not\*\* key|not the HTTP read-through",
        must_flag=[
            "Purge is available in-process, with keys attached at insert via `InsertBuilder::surrogate_keys`. Rollback is therefore flip then purge.",
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
        for bad in c.must_flag:
            if not c.hits(normalize(bad)):
                broken.append(f"  {c.name!r}: FAILED to flag known-bad text:\n    {bad[:110]}")
        for good in c.must_pass:
            if c.hits(normalize(good)):
                broken.append(f"  {c.name!r}: wrongly flagged corrected text:\n    {good[:110]}")
    return broken


def main():
    broken = self_test()
    if broken:
        print("CHECKER IS BROKEN — its own fixtures do not pass.")
        print("A green run from this state would be meaningless.\n")
        print("\n".join(broken))
        return 2

    fail = False
    for c in CHECKS:
        found = []
        for path in targets():
            for h in c.hits(normalize(open(path).read())):
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
