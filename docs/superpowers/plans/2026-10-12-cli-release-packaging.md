# CLI Release Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pushing a `v*` tag builds `ts` for Linux and macOS on x86_64 and
arm64, packages it as `.deb`, `.rpm`, `.pkg.tar.zst`, and `.tar.gz`, smoke
tests every package, and publishes them to a GitHub Release.

**Architecture:** Shell helpers in `scripts/release/` hold all logic that can
be tested locally: tag validation, release notes extraction, and Linux
packaging. `.github/workflows/release.yml` wires them into the jobs `verify`,
`build`, `package-linux`, `package-macos`, `smoke`, and `publish`. A single
`packaging/nfpm.yaml` produces all three Linux formats.

**Tech Stack:** GitHub Actions, bash, nfpm 2.43.0, `gh` CLI,
`actions/attest-build-provenance`, Docker (smoke tests).

**Spec:** `docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md`

## Global Constraints

- Tags match `v<semver>`, and `<semver>` equals the `trusted-server-cli`
  version from `cargo metadata`. Any other tag fails before builds start.
- Targets: `x86_64-unknown-linux-gnu` on `ubuntu-22.04`,
  `aarch64-unknown-linux-gnu` on `ubuntu-22.04-arm`, and both
  `aarch64-apple-darwin` and `x86_64-apple-darwin` on `macos-14`.
- Package name `trusted-server-cli`. The binary installs to `/usr/bin/ts`,
  mode `0755`. There are no package dependencies.
- Workflow default `permissions: contents: read`. Only `publish` gets
  `contents: write`, `id-token: write`, and `attestations: write`.
- Actions are pinned by full commit SHA with the tag in a trailing comment,
  as #1235 requires. The nfpm download is pinned to `2.43.0` and checksum-verified.
- No real domains other than this repository's own GitHub URL. Test fixtures
  use example values only.
- Commits use sentence case, imperative mood, no prefixes, and `--signoff -S`.

## Related issues

- Implements #863.
- Shares `release.yml` with #1235, which adds the attested wasm and SBOMs
  later. Keep SHA pinning, the draft-then-publish release, and `--locked`
  builds so those jobs can be added without restructuring.
- Before the first real release, a maintainer enables immutable releases in
  the repository settings and adds a tag ruleset for `v*`.

## Precondition for the first real release (not part of this plan)

`CHANGELOG.md` already has `## [1.2.0]` while `workspace.package.version` is
`0.1.0`. Whoever cuts the first release must set the workspace version to a
value greater than both, for example `1.3.0`, in its own PR.

## File structure

| File                                      | Responsibility                                                 |
| ----------------------------------------- | -------------------------------------------------------------- |
| `scripts/release/check-version.sh`        | Validate a tag against the manifest version, print the version |
| `scripts/release/release-notes.sh`        | Print one changelog section followed by install instructions   |
| `scripts/release/package-linux.sh`        | Run nfpm for deb, rpm, and archlinux for one architecture      |
| `scripts/release/test-release-scripts.sh` | Bash tests for the two pure scripts above                      |
| `scripts/release/fixtures/CHANGELOG.md`   | Fixture changelog for the tests                                |
| `packaging/nfpm.yaml`                     | Linux package definition                                       |
| `packaging/install.md`                    | Install instructions template, `@VERSION@` placeholder         |
| `.github/workflows/release.yml`           | Release pipeline                                               |
| `CONTRIBUTING.md`                         | "Cutting a release" section                                    |

---

### Task 1: Tag validation and release notes scripts

**Files:**

- Create: `scripts/release/check-version.sh`
- Create: `scripts/release/release-notes.sh`
- Create: `packaging/install.md`
- Create: `scripts/release/fixtures/CHANGELOG.md`
- Test: `scripts/release/test-release-scripts.sh`

**Interfaces:**

- Produces: `check-version.sh <tag> <manifest-version>` prints `<version>`
  to stdout and exits 0, or prints an error to stderr and exits 1.
- Produces: `release-notes.sh <changelog-path> <section> <version>` prints
  the body of `## [<section>]` (heading excluded), a blank line, then
  `packaging/install.md` with every `@VERSION@` replaced by `<version>`.
  Exits 1 if the section is missing or has no non-blank lines.

- [ ] **Step 1: Write the fixture changelog**

`scripts/release/fixtures/CHANGELOG.md`:

```markdown
# Changelog

## [Unreleased]

### Added

- Pending change.

## [1.3.0] - 2026-01-01

### Fixed

- Example fix one.
- Example fix two.

## [1.2.0] - 2025-10-14

### Added

- Older change.

## [1.1.0] - 2025-10-05
```

- [ ] **Step 2: Write the failing tests**

`scripts/release/test-release-scripts.sh`:

```bash
#!/usr/bin/env bash
# Tests for the release helper scripts. Run from anywhere.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="$DIR/fixtures/CHANGELOG.md"
FAILURES=0

pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1" >&2; FAILURES=$((FAILURES + 1)); }

expect_ok() {
  local name="$1" expected="$2"
  shift 2
  local actual
  if actual="$("$@" 2>/dev/null)" && [ "$actual" = "$expected" ]; then
    pass "$name"
  else
    fail "$name (got: ${actual:-<error>})"
  fi
}

expect_err() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then fail "$name (expected failure)"; else pass "$name"; fi
}

# check-version.sh
expect_ok "accepts matching release tag" "1.3.0" "$DIR/check-version.sh" v1.3.0 1.3.0
expect_ok "accepts matching prerelease tag" "1.3.0-rc.1" "$DIR/check-version.sh" v1.3.0-rc.1 1.3.0-rc.1
expect_err "rejects tag without v prefix" "$DIR/check-version.sh" 1.3.0 1.3.0
expect_err "rejects non-semver tag" "$DIR/check-version.sh" v1.3 1.3
expect_err "rejects mismatched tag" "$DIR/check-version.sh" v1.3.1 1.3.0
expect_err "rejects missing arguments" "$DIR/check-version.sh" v1.3.0

# release-notes.sh
NOTES="$("$DIR/release-notes.sh" "$FIXTURE" 1.3.0 1.3.0)"
if grep -qx -- "- Example fix two." <<<"$NOTES" && ! grep -q "Older change" <<<"$NOTES"; then
  pass "extracts only the requested section"
else
  fail "extracts only the requested section"
fi
if ! grep -q "^## \[1.3.0\]" <<<"$NOTES"; then pass "omits section heading"; else fail "omits section heading"; fi
if grep -q "trusted-server-cli_1.3.0_amd64.deb" <<<"$NOTES" && ! grep -q "@VERSION@" <<<"$NOTES"; then
  pass "appends install instructions with version"
else
  fail "appends install instructions with version"
fi
expect_ok "extracts Unreleased section first line" "### Added" \
  bash -c "\"$DIR/release-notes.sh\" \"$FIXTURE\" Unreleased 0.0.0 | sed -n 1p"
expect_err "rejects missing section" "$DIR/release-notes.sh" "$FIXTURE" 9.9.9 9.9.9
expect_err "rejects empty section" "$DIR/release-notes.sh" "$FIXTURE" 1.1.0 1.1.0

if [ "$FAILURES" -ne 0 ]; then
  echo "$FAILURES test(s) failed" >&2
  exit 1
fi
echo "all release script tests passed"
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `chmod +x scripts/release/test-release-scripts.sh && scripts/release/test-release-scripts.sh`
Expected: exit 1 with `FAIL` lines, because neither script exists.

- [ ] **Step 4: Implement `check-version.sh`**

```bash
#!/usr/bin/env bash
# Validate a release tag against the manifest version.
#
# Usage: check-version.sh <tag> <manifest-version>
# Prints the version (tag without the leading `v`) on success.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: check-version.sh <tag> <manifest-version>" >&2
  exit 1
fi

TAG="$1"
MANIFEST_VERSION="$2"
SEMVER_RE='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'

if ! [[ "$TAG" =~ $SEMVER_RE ]]; then
  echo "tag '$TAG' is not of the form v<major>.<minor>.<patch>[-<prerelease>]" >&2
  exit 1
fi

VERSION="${TAG#v}"
if [ "$VERSION" != "$MANIFEST_VERSION" ]; then
  echo "tag version '$VERSION' does not match manifest version '$MANIFEST_VERSION'" >&2
  exit 1
fi

echo "$VERSION"
```

- [ ] **Step 5: Implement `packaging/install.md`**

```markdown
## Install

- Ubuntu: `sudo apt install ./trusted-server-cli_@VERSION@_amd64.deb`
  (use `_arm64.deb` on arm64)
- Fedora: `sudo dnf install ./trusted-server-cli-@VERSION@-1.x86_64.rpm`
  (use `.aarch64.rpm` on arm64)
- Arch: `sudo pacman -U trusted-server-cli-@VERSION@-1-x86_64.pkg.tar.zst`
  (use `-aarch64.pkg.tar.zst` on arm64)
- macOS: extract `ts-@VERSION@-aarch64-apple-darwin.tar.gz` (Apple Silicon)
  or `ts-@VERSION@-x86_64-apple-darwin.tar.gz` (Intel) and move `ts` onto
  your `PATH`. The binary is not signed. If it was downloaded through a
  browser, run `xattr -d com.apple.quarantine ts` once.

## Verify

- `sha256sum -c SHA256SUMS --ignore-missing`
- `gh attestation verify <file> --repo IABTechLab/trusted-server`
```

Prerelease versions such as `1.3.0-rc.1` appear in package file names in
each format's own syntax (for example `1.3.0~rc.1`). The `@VERSION@` lines
show the plain release form, and that's acceptable for prereleases.

- [ ] **Step 6: Implement `release-notes.sh`**

```bash
#!/usr/bin/env bash
# Print one CHANGELOG.md section followed by install instructions.
#
# Usage: release-notes.sh <changelog-path> <section> <version>
# <section> is the bracketed heading name, e.g. `1.3.0` or `Unreleased`.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: release-notes.sh <changelog-path> <section> <version>" >&2
  exit 1
fi

CHANGELOG="$1"
SECTION="$2"
VERSION="$3"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALL="$REPO_ROOT/packaging/install.md"

BODY="$(awk -v heading="## [$SECTION]" '
  index($0, heading) == 1 { found = 1; next }
  found && /^## \[/ { exit }
  found { print }
' "$CHANGELOG")"

if [ -z "$(tr -d '[:space:]' <<<"$BODY")" ]; then
  echo "CHANGELOG section [$SECTION] is missing or empty" >&2
  exit 1
fi

# Trim leading and trailing blank lines from the section body.
sed -e '/./,$!d' <<<"$BODY" | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}'
echo
sed "s/@VERSION@/$VERSION/g" "$INSTALL"
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `chmod +x scripts/release/*.sh && scripts/release/test-release-scripts.sh`
Expected: every line starts with `ok`, the last line is
`all release script tests passed`, and it exits 0.

- [ ] **Step 8: Run against the real changelog**

Run: `scripts/release/release-notes.sh CHANGELOG.md 1.2.0 1.2.0 | head -5`
Expected: the first lines of the real `[1.2.0]` section, with no heading.

- [ ] **Step 9: Commit**

```bash
git add scripts/release packaging/install.md
git commit --signoff -S -m "Add release tag validation and release notes scripts"
```

---

### Task 2: Linux packaging with nfpm

**Files:**

- Create: `packaging/nfpm.yaml`
- Create: `scripts/release/package-linux.sh`

**Interfaces:**

- Consumes: nothing from Task 1.
- Produces: `package-linux.sh <binary> <version> <arch> <outdir>`, where
  `<arch>` is `amd64` or `arm64`. Writes exactly three files to `<outdir>`
  with nfpm's default names:
  `trusted-server-cli_<v>_<arch>.deb`,
  `trusted-server-cli-<v>-1.<x86_64|aarch64>.rpm`,
  `trusted-server-cli-<v>-1-<x86_64|aarch64>.pkg.tar.zst`.
  Requires `nfpm` on `PATH`. Run it from the repository root.

- [ ] **Step 1: Write `packaging/nfpm.yaml`**

```yaml
# Linux package definition for the `ts` operator CLI.
# Built by scripts/release/package-linux.sh; see
# docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md.
name: trusted-server-cli
arch: ${ARCH}
platform: linux
version: ${VERSION}
version_schema: semver
release: 1
section: utils
priority: optional
maintainer: Trusted Server maintainers
description: Operator CLI (`ts`) for Trusted Server.
homepage: https://github.com/IABTechLab/trusted-server
license: Apache-2.0

contents:
  - src: ${BINARY}
    dst: /usr/bin/ts
    file_info:
      mode: 0755
  - src: LICENSE
    dst: /usr/share/licenses/trusted-server-cli/LICENSE
    file_info:
      mode: 0644
  - src: README.md
    dst: /usr/share/doc/trusted-server-cli/README.md
    file_info:
      mode: 0644
```

- [ ] **Step 2: Write `scripts/release/package-linux.sh`**

```bash
#!/usr/bin/env bash
# Build deb, rpm, and archlinux packages of `ts` for one architecture.
#
# Usage: package-linux.sh <binary> <version> <amd64|arm64> <outdir>
# Run from the repository root. Requires `nfpm` on PATH.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: package-linux.sh <binary> <version> <amd64|arm64> <outdir>" >&2
  exit 1
fi

case "$3" in
  amd64 | arm64) ;;
  *)
    echo "unsupported arch '$3'; expected amd64 or arm64" >&2
    exit 1
    ;;
esac

export BINARY="$1" VERSION="$2" ARCH="$3"
OUTDIR="$4"
mkdir -p "$OUTDIR"

for PACKAGER in deb rpm archlinux; do
  nfpm package --config packaging/nfpm.yaml --packager "$PACKAGER" --target "$OUTDIR"
done
```

- [ ] **Step 3: Verify packaging locally**

Run from the repository root (Docker provides nfpm, so nothing gets
installed on the host):

```bash
chmod +x scripts/release/package-linux.sh
HOST="$(rustc -vV | sed -n 's/host: //p')"
cargo build -p trusted-server-cli --release --locked --target "$HOST"
rm -rf /tmp/pkgtest && mkdir /tmp/pkgtest
docker run --rm -v "$PWD:/src" -w /src --entrypoint sh goreleaser/nfpm:v2.43.0 \
  -c "scripts/release/package-linux.sh target/$HOST/release/ts 1.3.0-rc.1 amd64 /src/target/pkgtest"
ls target/pkgtest
```

Expected: exactly three files ending in `_amd64.deb`, `.x86_64.rpm`, and
`-x86_64.pkg.tar.zst`. The goreleaser image is Alpine and has `sh` but not
`bash`. If the script fails to start, run the three `nfpm package` commands
inline instead. The workflow runs the script with bash on Ubuntu.

- [ ] **Step 4: Inspect one package**

Run: `docker run --rm -v "$PWD/target/pkgtest:/p" ubuntu:24.04 bash -c 'apt-get install -y /p/*.deb >/dev/null && ls -l /usr/bin/ts && ts --version'`
Expected: `-rwxr-xr-x ... /usr/bin/ts` and a version line. This step
assumes an x86_64 Linux host with a glibc no newer than the container's. On
other hosts, skip it; the `smoke` job covers it.

- [ ] **Step 5: Commit**

```bash
git add packaging/nfpm.yaml scripts/release/package-linux.sh
git commit --signoff -S -m "Add nfpm packaging for the ts CLI on Linux"
```

---

### Task 3: Release workflow

**Files:**

- Create: `.github/workflows/release.yml`

**Interfaces:**

- Consumes: `check-version.sh`, `release-notes.sh` (Task 1),
  `package-linux.sh`, `packaging/nfpm.yaml` (Task 2).
- Produces: workflow artifacts `release-notes`, `ts-<triple>`,
  `pkg-linux-<arch>`, `pkg-macos-<triple>`. `publish` runs only on tag push.

- [ ] **Step 1: Write the workflow**

```yaml
name: Release

on:
  push:
    tags: ['v*']
  # Dry run: build, package, and smoke test without publishing.
  pull_request:
    paths:
      - '.github/workflows/release.yml'
      - 'packaging/**'
      - 'scripts/release/**'

permissions:
  contents: read

concurrency:
  group: release-${{ github.ref }}
  cancel-in-progress: false

env:
  NFPM_VERSION: 2.43.0

jobs:
  verify:
    name: Verify tag, notes, and CLI tests
    runs-on: ubuntu-22.04
    outputs:
      version: ${{ steps.version.outputs.version }}
      prerelease: ${{ steps.version.outputs.prerelease }}
    steps:
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4

      - name: Retrieve Rust version
        id: rust-version
        run: echo "rust-version=$(grep '^rust ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
        shell: bash

      - name: Set up Rust toolchain
        uses: actions-rust-lang/setup-rust-toolchain@166cdcfd11aee3cb47222f9ddb555ce30ddb9659 # v1
        with:
          toolchain: ${{ steps.rust-version.outputs.rust-version }}
          cache-shared-key: cargo-cli-${{ runner.os }}

      - name: Release script tests
        run: scripts/release/test-release-scripts.sh

      - name: Resolve version
        id: version
        shell: bash
        run: |
          MANIFEST="$(cargo metadata --no-deps --format-version 1 \
            | jq -r '.packages[] | select(.name == "trusted-server-cli") | .version')"
          if [ "$GITHUB_EVENT_NAME" = "push" ]; then TAG="$GITHUB_REF_NAME"; else TAG="v$MANIFEST"; fi
          VERSION="$(scripts/release/check-version.sh "$TAG" "$MANIFEST")"
          echo "version=$VERSION" >> "$GITHUB_OUTPUT"
          if [[ "$VERSION" == *-* ]]; then echo "prerelease=true"; else echo "prerelease=false"; fi >> "$GITHUB_OUTPUT"

      - name: Extract release notes
        shell: bash
        env:
          VERSION: ${{ steps.version.outputs.version }}
        run: |
          if [ "$GITHUB_EVENT_NAME" = "push" ]; then SECTION="$VERSION"; else SECTION="Unreleased"; fi
          scripts/release/release-notes.sh CHANGELOG.md "$SECTION" "$VERSION" > release-notes.md

      - uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4
        with:
          name: release-notes
          path: release-notes.md

      - name: Read pinned Node.js version for CLI fixtures
        id: node-version
        shell: bash
        run: echo "version=$(awk '$1 == "nodejs" { print $2 }' .tool-versions)" >> "$GITHUB_OUTPUT"

      - uses: actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020 # v4
        with:
          node-version: ${{ steps.node-version.outputs.version }}

      - name: cargo test (ts CLI)
        run: |
          cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')"

  build:
    name: Build ${{ matrix.target }}
    needs: verify
    strategy:
      fail-fast: true
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-22.04
          - target: aarch64-unknown-linux-gnu
            runner: ubuntu-22.04-arm
          - target: aarch64-apple-darwin
            runner: macos-14
          # GitHub has retired Intel macOS runners; cross compile on Apple Silicon.
          - target: x86_64-apple-darwin
            runner: macos-14
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4

      - name: Retrieve Rust version
        id: rust-version
        run: echo "rust-version=$(grep '^rust ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
        shell: bash

      - name: Set up Rust toolchain
        uses: actions-rust-lang/setup-rust-toolchain@166cdcfd11aee3cb47222f9ddb555ce30ddb9659 # v1
        with:
          toolchain: ${{ steps.rust-version.outputs.rust-version }}
          target: ${{ matrix.target }}
          cache-shared-key: cargo-release-${{ matrix.target }}

      - name: cargo build
        run: cargo build -p trusted-server-cli --release --locked --target ${{ matrix.target }}

      - name: Strip binary
        run: strip target/${{ matrix.target }}/release/ts

      - uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4
        with:
          name: ts-${{ matrix.target }}
          path: target/${{ matrix.target }}/release/ts
          if-no-files-found: error

  package-linux:
    name: Package Linux ${{ matrix.arch }}
    needs: [verify, build]
    strategy:
      matrix:
        include:
          - arch: amd64
            target: x86_64-unknown-linux-gnu
          - arch: arm64
            target: aarch64-unknown-linux-gnu
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4

      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          name: ts-${{ matrix.target }}
          path: bin

      - name: Install nfpm
        run: |
          BASE="https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}"
          TARBALL="nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz"
          curl -fsSLO "$BASE/$TARBALL"
          curl -fsSLO "$BASE/checksums.txt"
          grep " ${TARBALL}\$" checksums.txt | sha256sum -c -
          tar -xzf "$TARBALL" nfpm
          sudo install nfpm /usr/local/bin/nfpm

      - name: Build packages
        run: |
          chmod 0755 bin/ts
          scripts/release/package-linux.sh bin/ts "${{ needs.verify.outputs.version }}" ${{ matrix.arch }} dist

      - uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4
        with:
          name: pkg-linux-${{ matrix.arch }}
          path: dist/*
          if-no-files-found: error

  package-macos:
    name: Package macOS ${{ matrix.target }}
    needs: [verify, build]
    strategy:
      matrix:
        target: [aarch64-apple-darwin, x86_64-apple-darwin]
    runs-on: macos-14
    steps:
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4

      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          name: ts-${{ matrix.target }}
          path: bin

      - name: Build archive
        env:
          VERSION: ${{ needs.verify.outputs.version }}
        run: |
          chmod 0755 bin/ts
          mkdir dist
          tar -czf "dist/ts-${VERSION}-${{ matrix.target }}.tar.gz" -C bin ts -C .. LICENSE README.md

      - name: Smoke test archive
        env:
          VERSION: ${{ needs.verify.outputs.version }}
        run: |
          if [ "${{ matrix.target }}" = "x86_64-apple-darwin" ]; then
            softwareupdate --install-rosetta --agree-to-license || true
          fi
          mkdir smoke
          tar -xzf dist/*.tar.gz -C smoke
          smoke/ts --version | tee /dev/stderr | grep -F "$VERSION"

      - uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4
        with:
          name: pkg-macos-${{ matrix.target }}
          path: dist/*
          if-no-files-found: error

  smoke:
    name: Smoke ${{ matrix.image }} ${{ matrix.arch }}
    needs: [verify, package-linux]
    strategy:
      fail-fast: false
      matrix:
        include:
          - {
              arch: amd64,
              runner: ubuntu-22.04,
              image: 'ubuntu:22.04',
              glob: '*.deb',
              install: 'apt-get install -y',
            }
          - {
              arch: amd64,
              runner: ubuntu-22.04,
              image: 'ubuntu:24.04',
              glob: '*.deb',
              install: 'apt-get install -y',
            }
          - {
              arch: arm64,
              runner: ubuntu-22.04-arm,
              image: 'ubuntu:22.04',
              glob: '*.deb',
              install: 'apt-get install -y',
            }
          - {
              arch: arm64,
              runner: ubuntu-22.04-arm,
              image: 'ubuntu:24.04',
              glob: '*.deb',
              install: 'apt-get install -y',
            }
          - {
              arch: amd64,
              runner: ubuntu-22.04,
              image: 'fedora:latest',
              glob: '*.rpm',
              install: 'dnf install -y',
            }
          - {
              arch: arm64,
              runner: ubuntu-22.04-arm,
              image: 'fedora:latest',
              glob: '*.rpm',
              install: 'dnf install -y',
            }
          - {
              arch: amd64,
              runner: ubuntu-22.04,
              image: 'archlinux:latest',
              glob: '*.pkg.tar.zst',
              install: 'pacman -U --noconfirm',
            }
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          name: pkg-linux-${{ matrix.arch }}
          path: dist

      - name: Install and run ts
        env:
          VERSION: ${{ needs.verify.outputs.version }}
        run: |
          docker run --rm -v "$PWD/dist:/dist" "${{ matrix.image }}" bash -c \
            "${{ matrix.install }} /dist/${{ matrix.glob }} >/dev/null && test -x /usr/bin/ts && ts --version" \
            | tee /dev/stderr | grep -F "$VERSION"

  smoke-arch-arm64:
    name: Check arm64 Arch package metadata
    needs: package-linux
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          name: pkg-linux-arm64
          path: dist

      # archlinux:latest has no official arm64 image, so check metadata only.
      - name: Check .PKGINFO arch
        run: tar --zstd -xOf dist/*.pkg.tar.zst .PKGINFO | grep -x "arch = aarch64"

  publish:
    name: Publish GitHub Release
    if: github.event_name == 'push'
    needs: [verify, package-linux, package-macos, smoke, smoke-arch-arm64]
    runs-on: ubuntu-22.04
    permissions:
      contents: write
      id-token: write
      attestations: write
    steps:
      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          pattern: pkg-*
          path: dist
          merge-multiple: true

      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093 # v4
        with:
          name: release-notes

      - name: Write checksums
        working-directory: dist
        run: sha256sum * > SHA256SUMS && cat SHA256SUMS

      - uses: actions/attest-build-provenance@e8998f949152b193b063cb0ec769d69d929409be # v2
        with:
          subject-path: 'dist/*'

      - name: Create draft release
        env:
          GH_TOKEN: ${{ github.token }}
          TAG: ${{ github.ref_name }}
          PRERELEASE: ${{ needs.verify.outputs.prerelease }}
        run: |
          FLAGS=()
          if [ "$PRERELEASE" = "true" ]; then FLAGS+=(--prerelease); fi
          gh release create "$TAG" --repo "$GITHUB_REPOSITORY" --draft --title "$TAG" \
            --notes-file release-notes.md --verify-tag "${FLAGS[@]}" dist/*

      # Publishing makes the release immutable once immutable releases are on.
      - name: Publish release
        env:
          GH_TOKEN: ${{ github.token }}
          TAG: ${{ github.ref_name }}
        run: gh release edit "$TAG" --repo "$GITHUB_REPOSITORY" --draft=false
```

- [ ] **Step 2: Lint the workflow**

Run: `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:1.7.7 -color .github/workflows/release.yml`
Expected: no output and exit 0. If shellcheck reports warnings in `run:`
blocks, fix them. Don't add disables.

- [ ] **Step 3: Format check**

Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check .github/workflows/release.yml packaging/install.md`
Expected: all files pass. If any fail, run with `--write`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit --signoff -S -m "Add tag-triggered release workflow for the ts CLI"
```

- [ ] **Step 5: Exercise the dry run on the PR**

Push the branch and open the implementation PR, based on the spec branch.
The `Release` workflow runs because `release.yml` changed.
Expected: `verify`, all four `build` jobs, both `package-*` jobs, all seven
`smoke` jobs, and `smoke-arch-arm64` pass. `publish` is skipped. Download
`pkg-linux-amd64` from the run and confirm it holds three files.

---

### Task 4: Release docs

**Files:**

- Modify: `CONTRIBUTING.md` (append a section)

- [ ] **Step 1: Append to `CONTRIBUTING.md`**

```markdown
## Cutting a release

Releases publish the `ts` CLI as Linux and macOS packages through
`.github/workflows/release.yml`.

1. In a PR, set `workspace.package.version` in the root `Cargo.toml` to the
   new version and rename `## [Unreleased]` in `CHANGELOG.md` to
   `## [<version>] - <YYYY-MM-DD>`, adding a fresh empty `[Unreleased]`
   section above it.
2. Merge the PR to `main`.
3. Tag the merge commit and push the tag:
   `git tag -s v<version> -m v<version> && git push origin v<version>`.
4. Watch the `Release` workflow. On success a GitHub Release with packages,
   `SHA256SUMS`, and provenance attestations appears.

Use a prerelease tag such as `v<version>-rc.1` to rehearse; it publishes a
GitHub prerelease. The tag must match the manifest version exactly, so bump
the manifest to the rc version first.

If `publish` fails before the draft is published, delete the draft and rerun
the workflow. Published releases are immutable and versions are never
reused: to retract one, edit its notes to point to the fix and release a new
patch version.
```

- [ ] **Step 2: Format check**

Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CONTRIBUTING.md && cd docs && npm run format`
Expected: both pass.

- [ ] **Step 3: Commit**

```bash
git add CONTRIBUTING.md
git commit --signoff -S -m "Document cutting a release of the ts CLI"
```
