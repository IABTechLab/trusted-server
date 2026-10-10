#!/usr/bin/env python3
"""Check that every CI job pins Node before it compiles Rust.

trusted-server-core depends on trusted-server-js, whose build script runs the
TSJS build with the `npm` on PATH. A job that compiles core before setting up
the `.tool-versions` Node builds the embedded bundles with the runner image's
Node instead (see #1204).

For each job in `.github/workflows/*.yml`, the check walks the steps in order,
inlining local composite actions (`uses: ./...`). It fails when a step that
compiles Rust comes before an unconditional `actions/setup-node` step pinned
to `.tool-versions`. A step compiles Rust when it runs a `cargo` build, check,
test, clippy, bench, run or doc command (including the `cargo <command>-<name>`
aliases) or `worker-build`, directly or through a repository shell script.
A compile step counts even when it has an `if:`, but a conditional setup-node
step does not pin Node. A `node-version` taken from a step output counts as
pinned, since the workflows read that output from `.tool-versions`.

The check uses only the standard library and reads YAML by indentation, so it
expects the block-style YAML the repository's workflows use.
"""

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import sys

COMPILE = re.compile(
    r"\bcargo\s+(?:build|check|test|clippy|bench|run|doc|rustc)(?:-[\w-]+)?\b"
    r"|\bworker-build\b"
)
SCRIPT = re.compile(r"(?<![\w/.-])(?:\./)?((?:scripts|crates|\.github)/[\w./-]+\.sh)\b")
KEY = re.compile(r"^([\w.-]+):(?:\s+(.*))?$")
STEP_OUTPUT = re.compile(r"^\$\{\{\s*steps\.[\w-]+\.outputs\.[\w-]+\s*\}\}$")


class ParseError(Exception):
    """Raised when a workflow or action does not have the expected shape."""


@dataclass
class Entry:
    """A mapping key, its inline value and the line range of its block."""

    line: int
    value: str
    start: int
    end: int


@dataclass
class Step:
    """A job step after composite actions are inlined."""

    path: Path
    line: int
    name: str
    pins_node: bool
    compiles: bool


def is_content(line):
    stripped = line.strip()
    return bool(stripped) and not stripped.startswith("#")


def indent_of(line):
    return len(line) - len(line.lstrip(" "))


def first_content(lines, start, end):
    for index in range(start, end):
        if is_content(lines[index]):
            return index
    return None


def mapping(lines, start, end):
    """Return the keys at the shallowest indent of `lines[start:end]`."""
    first = first_content(lines, start, end)
    if first is None:
        return {}
    base = indent_of(lines[first])
    # A sequence may sit at its parent key's indent (`steps:` then `- run:`),
    # so its items belong to that key's block rather than ending it.
    heads = [
        index
        for index in range(first, end)
        if is_content(lines[index])
        and indent_of(lines[index]) <= base
        and not lines[index].lstrip().startswith("-")
    ]
    entries = {}
    for position, head in enumerate(heads):
        match = KEY.match(lines[head].strip())
        if match is None:
            continue
        stop = heads[position + 1] if position + 1 < len(heads) else end
        entries[match.group(1)] = Entry(head, (match.group(2) or "").strip(), head + 1, stop)
    return entries


def sequence(lines, start, end):
    """Return `(start, end)` for each item of the block sequence in range."""
    first = first_content(lines, start, end)
    if first is None:
        return []
    base = indent_of(lines[first])
    heads = [
        index
        for index in range(first, end)
        if is_content(lines[index])
        and indent_of(lines[index]) == base
        and lines[index].lstrip().startswith("-")
    ]
    return [
        (head, heads[position + 1] if position + 1 < len(heads) else end)
        for position, head in enumerate(heads)
    ]


def block_text(lines, entry):
    """Return the inline value and block lines of an entry, without comments."""
    parts = [entry.value]
    parts.extend(line for line in lines[entry.start : entry.end] if is_content(line))
    return "\n".join(parts)


def script_compiles(root, relative, seen):
    """Return whether a repository script runs a compile command."""
    path = root / relative
    if relative in seen or not path.is_file():
        return False
    seen.add(relative)
    text = "\n".join(line for line in path.read_text().splitlines() if is_content(line))
    if COMPILE.search(text):
        return True
    return any(script_compiles(root, nested, seen) for nested in SCRIPT.findall(text))


def runs_compile(root, text):
    if COMPILE.search(text):
        return True
    return any(script_compiles(root, relative, set()) for relative in SCRIPT.findall(text))


def pins_node(lines, step):
    """Return whether a setup-node step installs the `.tool-versions` Node."""
    if "with" not in step:
        return False
    options = mapping(lines, step["with"].start, step["with"].end)
    if "node-version-file" in options:
        return options["node-version-file"].value.strip("'\"") == ".tool-versions"
    if "node-version" in options:
        return STEP_OUTPUT.match(options["node-version"].value) is not None
    return False


def expand_steps(root, path, lines, start, end, conditional, active):
    """Yield the steps in range, inlining local composite actions."""
    for item_start, item_end in sequence(lines, start, end):
        # Read the item as a mapping by blanking out its `-` marker.
        item = list(lines)
        item[item_start] = item[item_start].replace("-", " ", 1)
        step = mapping(item, item_start, item_end)
        uses = step["uses"].value.strip("'\"") if "uses" in step else ""
        step_conditional = conditional or "if" in step
        if uses.startswith("./"):
            action = root / uses / "action.yml"
            if not action.is_file():
                action = root / uses / "action.yaml"
            if action in active:
                raise ParseError(f"{action}: composite action includes itself")
            yield from action_steps(root, action, step_conditional, active | {action})
            continue
        run = block_text(item, step["run"]) if "run" in step else ""
        yield Step(
            path=path,
            line=item_start + 1,
            name=step["name"].value if "name" in step else uses or run.strip().partition("\n")[0],
            pins_node=uses.startswith("actions/setup-node@")
            and not step_conditional
            and pins_node(item, step),
            compiles=runs_compile(root, run),
        )


def action_steps(root, path, conditional, active):
    lines = path.read_text().splitlines()
    top = mapping(lines, 0, len(lines))
    if "runs" not in top:
        raise ParseError(f"{path}: no `runs` key")
    runs = mapping(lines, top["runs"].start, top["runs"].end)
    if "steps" not in runs:
        return
    yield from expand_steps(
        root, path, lines, runs["steps"].start, runs["steps"].end, conditional, active
    )


def check_workflow(root, path):
    """Return the number of jobs checked and a violation message per bad job."""
    lines = path.read_text().splitlines()
    top = mapping(lines, 0, len(lines))
    if "jobs" not in top:
        raise ParseError(f"{path}: no `jobs` key")
    jobs = mapping(lines, top["jobs"].start, top["jobs"].end)
    if not jobs:
        raise ParseError(f"{path}: no jobs found")
    violations = []
    for job_id, job in jobs.items():
        keys = mapping(lines, job.start, job.end)
        if "steps" not in keys:
            continue
        pinned = False
        for step in expand_steps(
            root, path, lines, keys["steps"].start, keys["steps"].end, False, frozenset()
        ):
            if step.pins_node:
                pinned = True
            elif step.compiles and not pinned:
                violations.append(
                    f"{step.path.relative_to(root)}:{step.line}: job `{job_id}` in "
                    f"{path.relative_to(root)} compiles Rust in step `{step.name}` before "
                    "an unconditional actions/setup-node step pinned to .tool-versions"
                )
                break
    return len(jobs), violations


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root to check (default: this script's repository)",
    )
    root = parser.parse_args().root.resolve()
    workflows = sorted((root / ".github" / "workflows").glob("*.y*ml"))
    if not workflows:
        print(f"no workflows found under {root / '.github' / 'workflows'}", file=sys.stderr)
        return 1
    job_count = 0
    violations = []
    try:
        for workflow in workflows:
            count, found = check_workflow(root, workflow)
            job_count += count
            violations.extend(found)
    except ParseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    for violation in violations:
        print(violation, file=sys.stderr)
    if violations:
        print(f"{len(violations)} job(s) compile Rust before pinning Node", file=sys.stderr)
        return 1
    print(f"ok: {job_count} jobs in {len(workflows)} workflows pin Node before compiling Rust")
    return 0


if __name__ == "__main__":
    sys.exit(main())
