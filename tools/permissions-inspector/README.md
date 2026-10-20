# Permissions Inspector

A single-page tool that answers the question a policy owner actually
has about a `permissions.yaml`: for a visitor in a given place, with given
consent signals, which Data Uses are set?

No policy ever ships with Trusted Server: the builder of a deployment
chooses the `permissions.yaml` baked into their compiled image, the
operator of that image can overlay another, and the visitor's own
signals decide the rest at runtime. The repository carries sample
permissions files in `config/permissions/`, each naming itself with a
top-level `name:` line, and the page lists whatever that directory
holds. Choosing one is always explicit.

The page walks three steps. Step 1 is the policy baseline, one of the
repository's test policies (or a pasted or fetched file). Step 2 is the visitor's
input, being their location and the signals they carry (a TCF consent
string, Global Privacy Control). Step 3 is what the application layer
gets, the resulting permission per Data Use with the reason for each.

## The engine is the real code

Step 3 is computed by trusted-server-core itself, compiled to
WebAssembly and run in the page. The wrapper crate in `wasm/` exposes
the production path (`build_context_from_signals` decodes the raw
signals, `assemble_permissions` resolves the policy, and
`PermissionMaps::from_yaml` validates files with the server's own error
messages), so what the page shows is what the server does, and the page
states the trusted-server version, branch, commit and date the engine
was built from. A small JavaScript mirror provides the per-row
explanations, and the page reports how many Data Uses the mirror and
the Rust engine agree on, so any drift between them is visible rather
than silent.

## Running it

1. Build the engine (needs the `wasm32-unknown-unknown` target, added
   with `rustup target add wasm32-unknown-unknown`):

   ```bash
   ./scripts/build-inspector-wasm.sh
   ```

2. Serve the repository root with any local web server, so the page can
   fetch the samples and the built engine, then open:

   ```
   http://localhost:PORT/tools/permissions-inspector/index.html
   ```

The definitions behind each Data Use's help panel are quoted from the
IAB Tech Lab Privacy Taxonomy, and each panel links back to the
taxonomy repository as the authority.

## Later

Hosting this page through GitHub Pages would let it run in parallel
with any change to the repository, always reflecting a stated commit. A
companion editor for `permissions.yaml` is planned as its own change.
