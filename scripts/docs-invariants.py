#!/usr/bin/env python3
"""Cross-document invariant sweep for the #1009 document set.

`npm run format` and `npm run build` catch formatting and dead links. Neither
catches a claim corrected in one document and left standing in another, which is
the failure mode this set has hit repeatedly.

Checks are context-aware: a hit is excused only if an allowlist pattern appears
within a window of lines around it, because qualifying text usually wraps.

Exit 1 on any surviving hit.
"""
import re, sys, glob, os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
FILES = sorted(
    glob.glob(os.path.join(ROOT, "docs/superpowers/specs/2026-08-08-esi-*.md"))
    + glob.glob(os.path.join(ROOT, "docs/superpowers/plans/2026-08-*1009*.md"))
)

# (name, forbidden pattern, allowlist pattern or None, context window in lines)
CHECKS = [
    ("Stage 0 gate is three-verdict",
     r"PASS / FAIL|verdict = PASS[^A-Za-z]|subject to the `Vary` check",
     r"PROVISIONAL PASS / FAIL|FINAL PASS", 0),
    ("Rollback is not described as config-only",
     r"rollback is another config push rather", None, 0),
    ("adSlots is not in the shared template",
     r"two seams|adSlots[^.]*stays in the template|One per-user hole\.", None, 0),
    ("Headers finalize before assembly",
     r"finalization still run.{0,4} after assembly|run \*\*after\*\* assembly", None, 0),
    ("No nonexistent Core Cache APIs",
     r"(?<!into_)\bto_body\(\)", r"no `to_body|there is no", 2),
    ("A2/A3 not decided on root TTFB",
     r"beats A2 on TTFB", None, 0),
    ("ESI not described as impossible",
     r"ESI is not viable|structurally blocked", r"concluded that ESI was|original .* was wrong", 3),
    ("C1/C2 purge not conflated",
     r"InsertBuilder::surrogate_keys",
     r"Core Cache|C2|template cache|does \*\*not\*\* key|§6\.6", 4),
]

fail = False
for name, pat, allow, window in CHECKS:
    hits = []
    for path in FILES:
        lines = open(path).read().split("\n")
        for i, line in enumerate(lines):
            if not re.search(pat, line):
                continue
            lo, hi = max(0, i - window), min(len(lines), i + window + 1)
            ctx = "\n".join(lines[lo:hi])
            if allow and re.search(allow, ctx):
                continue
            hits.append(f"  {os.path.relpath(path, ROOT)}:{i+1}: {line.strip()[:100]}")
    if hits:
        fail = True
        print(f"\n[FAIL] {name}")
        print("\n".join(hits))
    else:
        print(f"[ok]   {name}")

print()
print("Contradictions found — do not push." if fail else "All invariants hold.")
sys.exit(1 if fail else 0)
