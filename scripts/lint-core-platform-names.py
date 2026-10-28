#!/usr/bin/env python3
"""Fail when a file under crates/trusted-server-core names a hosting platform.

Core is built on EdgeZero and names no hosting platform, in code, comments,
tests, documents or file names, so that one platform's change is never a
change in core. Each adapter names its own platform. This check reads every
file git tracks, or would track, under crates/trusted-server-core and fails on
any line or path that holds a platform's name or one of its domains.

Text is split into words at every character that is not a letter and at each
change from a lower case to an upper case letter, so `FASTLY_IS_STAGING`,
`FastlyStore`, `x-fastly-pop` and `adapter_fastly` each hold the word `fastly`.
A name in WHOLE_WORDS counts only as a word of its own, because each is also
part of ordinary words, so `spinner` holds no `spin`. A name in STEMS also
counts at the start of a word, so `akamaized` and `cloudflareinsights` count.
A domain in DOMAINS counts where it is not part of a longer host name, so
`example.workers.dev` counts and `notworkers.dev` does not. EdgeZero's own
names hold none of these.

Before it reads core, the check runs its matcher on every name and domain and
on known non-names, so a broken pattern stops the check rather than passing
everything.

Usage: python3 scripts/lint-core-platform-names.py
Exit status: 0 when core names no platform, 1 when it names one, 2 when the
check cannot run.
"""

import os
import re
import subprocess
import sys
import traceback
from pathlib import Path

CORE = "crates/trusted-server-core"

# Hosting platforms, and tools named for one, that count only as words of
# their own.
WHOLE_WORDS = ("aws", "axum", "deno", "gcore", "spin", "wrangler")

# Hosting platforms, and tools named for one, that also count at the start of
# a word.
STEMS = (
    "akamai",
    "amazonaws",
    "azure",
    "bunnycdn",
    "cloudflare",
    "cloudfront",
    "edgecast",
    "edgeworkers",
    "edgio",
    "fastly",
    "fermyon",
    "heroku",
    "miniflare",
    "netlify",
    "stackpath",
    "vercel",
    "viceroy",
    "workerd",
)

# Domains a platform serves its customers' deployments from.
DOMAINS = (
    "b-cdn.net",
    "bunny.net",
    "edg.io",
    "edgecompute.app",
    "edgekey.net",
    "edgesuite.net",
    "fermyon.app",
    "fly.dev",
    "fly.io",
    "llnwd.net",
    "netlify.app",
    "onrender.com",
    "pages.dev",
    "railway.app",
    "vercel.app",
    "workers.dev",
)

# Samples that hold nothing, or exactly the names given.
KNOWN_SAMPLES = (
    ("FASTLY_IS_STAGING", {"fastly"}),
    ("x-debug-fastly-pop", {"fastly"}),
    ("FastlyStore", {"fastly"}),
    ("trusted_server_adapter_fastly::app", {"fastly"}),
    ("edgezero_adapter_cloudflare::run_app", {"cloudflare"}),
    ("cdn.akamaized.net", {"akamai"}),
    ("Akamai EdgeWorkers", {"akamai", "edgeworkers"}),
    ("https://test.edgecompute.app/", {"edgecompute.app"}),
    ("a spinner, SpinyAI, steadfastly, a denominator, withdraws", set()),
    ("EdgeZero edge workers on notworkers.dev and butterfly.io", set()),
)

LETTERS = re.compile(r"[A-Za-z]+")
# An upper case run not followed by a lower case letter, such as an acronym,
# or a lower case run with at most one capital before it.
CAMEL = re.compile(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+")
DOMAIN_PATTERNS = tuple(
    (
        domain,
        re.compile(
            r"(?<![A-Za-z0-9-])" + re.escape(domain) + r"(?![A-Za-z0-9_-])",
            re.IGNORECASE,
        ),
    )
    for domain in DOMAINS
)
NEEDLES = WHOLE_WORDS + STEMS + DOMAINS

ADVICE = (
    "Core is built on EdgeZero and names no hosting platform. Say what the "
    "code needs from a platform without naming one, or move the code or test "
    "to the adapter for that platform."
)


def names_in(text):
    """Return the platform names and domains that `text` holds."""
    lower = text.lower()
    if not any(needle in lower for needle in NEEDLES):
        return []
    words = set()
    for run in LETTERS.findall(text):
        words.add(run.lower())
        words.update(word.lower() for word in CAMEL.findall(run))
    found = {name for name in WHOLE_WORDS if name in words}
    found.update(
        stem for stem in STEMS if any(word.startswith(stem) for word in words)
    )
    found.update(
        domain for domain, pattern in DOMAIN_PATTERNS if pattern.search(text)
    )
    return sorted(found)


def matcher_works():
    """Whether the matcher finds each name and domain and nothing it should not."""
    works = True

    def fail(sample, found, expected):
        print(
            f"the matcher found {sorted(found)} in {sample!r}, "
            f"where it should find {sorted(expected)}",
            file=sys.stderr,
        )

    for sample, expected in KNOWN_SAMPLES:
        found = set(names_in(sample))
        if found != expected:
            fail(sample, found, expected)
            works = False
    for name in WHOLE_WORDS + STEMS:
        sample = f"runs on {name.capitalize()}"
        found = set(names_in(sample))
        if name not in found:
            fail(sample, found, {name})
            works = False
    for domain in DOMAINS:
        sample = f"https://example.{domain}/path"
        found = set(names_in(sample))
        if domain not in found:
            fail(sample, found, {domain})
            works = False
    return works


def core_files(root):
    """List the files git tracks, or would track, under core."""
    listed = subprocess.run(
        [
            "git", "-C", str(root), "ls-files", "-z", "--cached", "--others",
            "--exclude-standard", "--", CORE,
        ],
        capture_output=True,
        check=True,
    ).stdout
    return sorted({path for path in listed.decode("utf-8").split("\0") if path})


def report(annotate, path, line, names, text):
    """Print one finding, with a GitHub annotation when running in Actions."""
    where = f"{path}:{line}" if line else path
    message = f"names {', '.join(names)}"
    print(f"{where}: {message}: {text[:160]}")
    if annotate:
        location = f"file={path},line={line}" if line else f"file={path}"
        print(f"::error {location}::{message}")


def main():
    # A finding quotes its line, which may hold any character, so the output
    # is UTF-8 whatever the console's own encoding.
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")

    if not matcher_works():
        print("the matcher is broken, so nothing was checked", file=sys.stderr)
        return 2

    root = Path(__file__).resolve().parent.parent
    try:
        paths = core_files(root)
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"cannot list the files under {CORE}: {error}", file=sys.stderr)
        return 2
    for required in ("Cargo.toml", "src/lib.rs"):
        if f"{CORE}/{required}" not in paths:
            print(f"found no {CORE}/{required} to check", file=sys.stderr)
            return 2

    annotate = os.environ.get("GITHUB_ACTIONS") == "true"
    checked = 0
    findings = 0
    for path in paths:
        file = root / path
        if not file.is_file():
            continue
        checked += 1
        found = names_in(path)
        if found:
            findings += 1
            report(annotate, path, None, found, path)
        text = file.read_bytes().decode("utf-8", errors="replace")
        for number, line in enumerate(text.split("\n"), 1):
            found = names_in(line)
            if found:
                findings += 1
                report(annotate, path, number, found, line.strip())

    if findings:
        places = "line or path" if findings == 1 else "lines or paths"
        verb = "names" if findings == 1 else "name"
        print(f"{findings} {places} in {CORE} {verb} a hosting platform.")
        print(ADVICE)
        return 1
    print(f"{CORE} names no hosting platform ({checked} files checked).")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:  # A fault in the check is not a finding, so exit 2.
        traceback.print_exc()
        sys.exit(2)
