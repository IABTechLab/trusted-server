# Package the `ts` CLI on production releases

- Issue: [#863](https://github.com/IABTechLab/trusted-server/issues/863)
- Related: [#1235](https://github.com/IABTechLab/trusted-server/issues/1235)
  (attested release binaries)
- Date: 2026-10-12
- Status: Proposed

## Goal

Pushing a release tag builds the `ts` operator CLI
(`crates/trusted-server-cli`) and publishes installable packages for macOS,
Ubuntu, Fedora, and Arch Linux on x86_64 and arm64 to a GitHub Release.

Today no workflow runs on tags, the repository has no release tags, and every
crate inherits `workspace.package.version` (`0.1.0`) from the root
`Cargo.toml`.

## Scope

In scope:

- A new `.github/workflows/release.yml` triggered by `v*` tag pushes.
- A new `packaging/nfpm.yaml` that describes the Linux packages.
- Release assets: Linux `.deb`, `.rpm`, and `.pkg.tar.zst`, macOS `.tar.gz`,
  `SHA256SUMS`, and build provenance attestations.

Out of scope:

- Homebrew tap and AUR feeds, and hosted apt or dnf repositories.
- macOS code signing and notarization.
- Windows builds.
- Automated version bumps or changelog generation.
- Packaging the adapters, the tsjs bundles, or any WASM artifact.
- SBOM generation and the attested wasm release, which belong to #1235.

## Relationship to #1235

#1235 plans a `.github/workflows/release.yml` that publishes attested wasm
binaries and SBOMs. This spec creates that workflow for the CLI first, and
follows the #1235 constraints so its wasm jobs can be added to the same file
without redesigning the pipeline:

- Actions are pinned by full commit SHA.
- Only GitHub-hosted runners are used.
- Releases are created as drafts, receive every asset, and are published
  only once all assets have been uploaded. With immutable releases enabled on
  the repository, published assets and tags cannot change.
- Builds use `--locked`.

## Trigger and versioning

The workflow runs on `push` of tags that match `v*`. The tag must be
`v<version>`, where `<version>` equals `workspace.package.version` in the root
`Cargo.toml`. A tag that is not semver, or that does not match the manifest,
fails the workflow before any build starts.

Releasing is a manual procedure. Bump `workspace.package.version`, add a
`## [<version>]` section to `CHANGELOG.md`, merge to `main`, then push the tag
from the merge commit.

A tag with a prerelease suffix (for example `v0.2.0-rc.1`) publishes a GitHub
prerelease. Every other tag publishes a normal release.

## Jobs

### 1. `verify`

Runs on `ubuntu-22.04`.

- Parse the tag and compare it with `cargo metadata` output for
  `trusted-server-cli`. Fail if they differ.
- Extract the matching `CHANGELOG.md` section to `release-notes.md`. Fail if
  the section is missing or empty. Upload it as a workflow artifact.
- Run the release script tests, then `cargo test` for the CLI on the host
  target, matching the Linux leg of `test.yml`. The Chrome-backed
  `scripts/test-cli.sh` fixtures stay in `test.yml` on macOS.

Every later job depends on `verify`, so a failed check publishes nothing.

### 2. `build` (matrix)

| Target                      | Runner             |
| --------------------------- | ------------------ |
| `x86_64-unknown-linux-gnu`  | `ubuntu-22.04`     |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |
| `aarch64-apple-darwin`      | `macos-14`         |
| `x86_64-apple-darwin`       | `macos-14`         |

Linux jobs and the arm64 macOS job build natively. The x86_64 macOS job
cross compiles on Apple Silicon, which needs only `rustup target add`, because
GitHub has retired its Intel macOS runners. Each job runs:

```bash
cargo build -p trusted-server-cli --release --locked --target <triple>
```

It strips the resulting `ts` and uploads it as artifact `ts-<triple>`. Linux
builds run on Ubuntu 22.04 so the binary links against glibc 2.35. That glibc
floor covers Ubuntu 22.04 and later, current Fedora, and Arch. The CLI uses
rustls and does not link OpenSSL, so it has no runtime TLS library dependency.

### 3. `package-linux` (matrix over x86_64 and arm64)

The job downloads `ts-<triple>`, installs a pinned `nfpm` release, and runs
`nfpm package` three times with the packagers `deb`, `rpm`, and `archlinux`.
`packaging/nfpm.yaml` reads `VERSION` and `ARCH` from the environment.

Package contents:

| Path                                             | Source      | Mode   |
| ------------------------------------------------ | ----------- | ------ |
| `/usr/bin/ts`                                    | built `ts`  | `0755` |
| `/usr/share/licenses/trusted-server-cli/LICENSE` | `LICENSE`   | `0644` |
| `/usr/share/doc/trusted-server-cli/README.md`    | `README.md` | `0644` |

Package metadata is name `trusted-server-cli`, the license from the workspace
manifest, the description from the CLI crate, and the repository homepage. The
packages declare no dependencies beyond libc.

Asset names follow each distribution's architecture naming convention:

| Format | x86_64                                          | arm64                                            |
| ------ | ----------------------------------------------- | ------------------------------------------------ |
| deb    | `trusted-server-cli_<ver>_amd64.deb`            | `trusted-server-cli_<ver>_arm64.deb`             |
| rpm    | `trusted-server-cli-<ver>-1.x86_64.rpm`         | `trusted-server-cli-<ver>-1.aarch64.rpm`         |
| arch   | `trusted-server-cli-<ver>-1-x86_64.pkg.tar.zst` | `trusted-server-cli-<ver>-1-aarch64.pkg.tar.zst` |

nfpm converts a semver prerelease into each format's own version syntax.

### 4. `package-macos` (matrix over both Apple targets)

Packs `ts`, `LICENSE`, and `README.md` into
`ts-<ver>-<triple>.tar.gz`. The binaries are not signed.

### 5. `smoke` (matrix)

Installs each Linux package in a clean container on the matching
architecture's runner and runs `ts --version`. The output must contain
`<version>`.

| Package | Container          | Install command         |
| ------- | ------------------ | ----------------------- |
| deb     | `ubuntu:22.04`     | `apt-get install ./…`   |
| deb     | `ubuntu:24.04`     | `apt-get install ./…`   |
| rpm     | `fedora:latest`    | `dnf install -y ./…`    |
| arch    | `archlinux:latest` | `pacman -U --noconfirm` |

`archlinux:latest` has no official arm64 image, so the Arch smoke runs on
x86_64 only. For the arm64 Arch package, a separate `smoke-arch-arm64` job
checks the `.PKGINFO` arch field instead.

The macOS jobs untar each archive and run `ts --version` on `macos-14`. The
x86_64 binary runs under Rosetta 2.

### 6. `publish`

Depends on `verify`, `package-linux`, `package-macos`, `smoke`, and
`smoke-arch-arm64`.

1. Download all package and archive artifacts.
2. Write `SHA256SUMS` with `sha256sum` over every asset.
3. Generate build provenance with `actions/attest-build-provenance` for every
   asset.
4. Create a draft release with every asset attached:

   ```bash
   gh release create "$TAG" --draft --title "$TAG" --notes-file release-notes.md --verify-tag [--prerelease] <assets> SHA256SUMS
   ```

5. Publish the draft. This is the step that makes the release immutable:

   ```bash
   gh release edit "$TAG" --draft=false
   ```

`release-notes.md` is the extracted changelog section followed by a fixed
install section:

- Ubuntu: `sudo apt install ./trusted-server-cli_<ver>_<arch>.deb`
- Fedora: `sudo dnf install ./trusted-server-cli-<ver>-1.<arch>.rpm`
- Arch: `sudo pacman -U trusted-server-cli-<ver>-1-<arch>.pkg.tar.zst`
- macOS: extract the archive, move `ts` onto `PATH`, and run
  `xattr -d com.apple.quarantine ts` if it was downloaded through a browser.
- Verification: `sha256sum -c SHA256SUMS` and
  `gh attestation verify <file> --repo <owner>/<repo>`.

## Security and permissions

- The workflow default is `permissions: contents: read`.
- Only `publish` gets `contents: write`, `id-token: write`, and
  `attestations: write`.
- Every action is pinned to a full commit SHA with the version tag in a
  trailing comment, as #1235 requires. The `nfpm` download is pinned to a version and its
  published checksum is verified.
- The release needs no repository secrets beyond the default `GITHUB_TOKEN`.
- Concurrency group `release-${{ github.ref }}` with `cancel-in-progress:
false`, so a release never runs twice for one tag.

## Failure handling

- Failures in `verify`, `build`, `package-*`, or `smoke` stop the run before
  `publish`, and no release is created.
- If `publish` fails before the draft is published, nothing is public. The
  operator deletes the draft and reruns the workflow for the same tag.
  `gh release create` fails fast if a release already exists for the tag, so
  assets are never silently mixed.
- A published release is immutable. To retract one, mark it as not latest
  and edit its notes to point to the fix, then release a new patch version.
  Published versions and tags are never reused.

## Testing the workflow

- **Pull request dry run.** `release.yml` also runs on `pull_request` when
  `.github/workflows/release.yml` or `packaging/**` changes. In this mode it
  uses `workspace.package.version` in place of a tag, runs every job except
  `publish`, and uploads the packages as workflow artifacts for inspection.
  Release notes come from the `[Unreleased]` changelog section.
- **Prerelease rehearsal.** The first real use is a `v<next>-rc.1` tag, which
  creates a GitHub prerelease that can be deleted.
- **Local check.** `nfpm package` with `packaging/nfpm.yaml` can be run on a
  developer machine against a host build of `ts`.

## Files

| File                            | Change                                  |
| ------------------------------- | --------------------------------------- |
| `.github/workflows/release.yml` | New                                     |
| `packaging/nfpm.yaml`           | New                                     |
| `packaging/install.md`          | New                                     |
| `scripts/release/*.sh`          | New                                     |
| `scripts/release/fixtures/`     | New                                     |
| `CONTRIBUTING.md`               | Add a short "Cutting a release" section |

No Rust source changes.
