# New Engineer Setup

Practical setup notes for your first days in the repository: how to read and
edit this documentation site, how to prove the first-party proxy works end to
end locally, and the build traps that cost new engineers the most time.

Read [Onboarding](/guide/onboarding) first. It covers the mental model, a trace
of a publisher request, the vocabulary used here, the code map, and a triage
map. [Getting Started](/guide/getting-started) covers prerequisites and running
an adapter. This page is the practical companion to both and does not repeat
them.

## Request access

Ask your manager or onboarding contact for these on your first day. Some take
time to be granted, so request them before you need them. Nothing else on this
page requires them, so you can start reading and building while you wait.

| Access                   | What it is for                                                                                    | Where                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| **Google account**       | Calendar invitations for team meetings, and shared documents                                      | Ask your manager or onboarding contact                                        |
| **GitHub account**       | Read and write access to the repository, so you can push branches and open pull requests          | [IABTechLab/trusted-server](https://github.com/IABTechLab/trusted-server)     |
| **GitHub project board** | The team's planned and in-flight work, and where you find a first task                            | [Trusted Server project board](https://github.com/orgs/IABTechLab/projects/3) |
| **Fastly account**       | The production edge platform. You need an account and an API token to deploy or inspect a service | [manage.fastly.com](https://manage.fastly.com)                                |

A Fastly account is only needed for deploying and inspecting real services.
Local development on the Axum adapter needs no edge account at all, so it is
the right place to start on day one.

## Reading these docs locally

This documentation is a [VitePress](https://vitepress.dev) site in `docs/`.
Running it locally gives you full-text search across every guide and lets you
preview a change before opening a pull request.

```bash
cd docs
npm ci
npm run dev
```

The site is served at `http://localhost:5173/trusted-server/`. The
`/trusted-server/` suffix matters: the site sets a `base` path for GitHub
Pages, so the bare `http://localhost:5173` redirects rather than serving the
home page. VitePress prints the correct URL when it starts. Pages reload as you
save. Stop it with `Ctrl+C`.

Each page is one Markdown file under `docs/guide/`, so the fastest way to find
the source of something you are reading is to search the repository for a
phrase from the page.

### Before you open a documentation pull request

```bash
npm run format:write   # apply Prettier formatting
npm run lint           # ESLint
npm run build          # production build; fails on broken internal links
```

`npm run build` is the one that matters most: it fails on a dead internal link,
so it catches a mistyped `/guide/...` path that would otherwise ship.
`npm run preview` serves the built output if you want to check the production
result.

Two conventions worth knowing before you edit a page:

- **Tool versions are not written literally.** Each entry in `.tool-versions`
  gets a double-brace placeholder named after the tool in upper case followed
  by `_VERSION`, and `docs/.vitepress/config.mts` substitutes it at build time.
  Use the placeholder rather than typing a version number, so the docs cannot
  drift from the pinned toolchain.
- **Diagrams are Mermaid.** Use a ` ```mermaid ` block; the site already
  configures the plugin. Keep node labels short. Mermaid sizes a node box to
  its explicit line breaks but not to its own text wrapping, so a long label
  renders clipped at the box border. Put the detail in a table beside the
  diagram instead.

## Prove the first-party proxy locally

A useful early exercise: run a local origin and fetch a file through the
first-party proxy, which exercises signing and proxying end to end.

```bash
# Terminal 1: serve a file from a local origin
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:9090
mkdir -p /tmp/ts-origin
printf 'hello from origin\n' > /tmp/ts-origin/hello.txt
python3 -m http.server 9090 --directory /tmp/ts-origin
```

```bash
# Terminal 2: with the server running, sign the URL
curl -s "http://127.0.0.1:7676/first-party/sign?url=http://localhost:9090/hello.txt"
```

Request the signed path that comes back; the response body should be
`hello from origin`. If signing fails, the proxy secret is usually missing from
the environment — see [Getting Started](/guide/getting-started).

## Pick a first issue

These three are scoped deliberately for a first contribution: each is small,
self-contained, has an existing test nearby to copy, and touches code that only
one caller depends on. Each issue carries its own reproduction, acceptance
criteria, and file-and-line pointers, so start by reading the issue in full.

| Issue                                                             | What it is                                                                      | Why it suits a first contribution                                                                                            |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| [#1093](https://github.com/IABTechLab/trusted-server/issues/1093) | Root Markdown escapes the Prettier gate, so several root pages fail `--check`   | Documentation and CI only, no runtime risk. A good way to learn the workflow and the format gates before changing behavior.  |
| [#1063](https://github.com/IABTechLab/trusted-server/issues/1063) | A bare `ts dev proxy` prints an internal error report instead of help           | One crate, one argument struct, an existing neighboring test. Teaches Clap and this repository's `error-stack` conventions.  |
| [#1144](https://github.com/IABTechLab/trusted-server/issues/1144) | Partner token placeholders from the config template are not rejected at startup | A single validation function with one production caller, and an adjacent branch to mirror. Teaches configuration validation. |

Take them in that order if you want the gentlest ramp: #1093 exercises the
review and merge mechanics with nothing at stake, then #1063 and #1144 are real
behavior changes of a similar small size.

Two notes before you start:

- **#1063 is macOS-only.** `ts dev proxy` has its dependencies scoped to macOS,
  so the subcommand does not exist on other hosts. It also needs an explicit
  host target, because the workspace default target is WebAssembly. Pick a
  different issue if you are not on a Mac.
- **#1093 edits `AGENTS.md`**, which is high-traffic. Check whether a large
  documentation pull request is open before you start, and rebase rather than
  forcing a conflict.

If all three are taken, the project board is the place to look next. Ask in the
team channel before starting anything unlabelled, since an issue that reads as
small often is not.

## Build traps

These are the failures most likely to cost you an afternoon. They are
environment problems, not code problems, and none of them produce an error
message that names its own cause.

| What you see                                                         | What is actually wrong                                                                                                                                                                                    |
| -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Bare `cargo test` fails while linking, with missing `fastly` symbols | The workspace mixes native and WebAssembly targets. Use the target-specific aliases in `.cargo/config.toml`, such as `cargo test-axum`. Note that bare `cargo check` succeeds, so it gives a false green. |
| `ts config validate` rejects a configuration file that looks correct | The installed `ts` predates a configuration-schema change. Re-run `cargo install-cli` after pulling.                                                                                                      |
| A Rust build fails inside a JavaScript step                          | The Rust build generates the browser bundles, so the pinned Node from `.tool-versions` is required even for Rust-only work. `dist/` is not checked in.                                                    |
| `ts` cannot find a manifest                                          | `edgezero.toml` uses repository-relative paths; run `ts` from the repository root.                                                                                                                        |
| `cargo test-fastly` fails on the runtime                             | Install the pinned simulator: `cargo install viceroy --version {{VICEROY_VERSION}} --locked --force`.                                                                                                     |
| Tests pass locally but CI fails                                      | Run `cargo fmt --all -- --check` and the target-matched clippy aliases. CI denies warnings.                                                                                                               |

For the full gate list, see `CLAUDE.md`. For which gate matches your change,
see [Testing](/guide/testing).
