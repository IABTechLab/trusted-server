# Native acceptance fixtures

`origin.rs` serves core's existing Next.js Flight fixture over local TCP. It also
records POST bodies, serves a deterministic API, and deliberately breaks one
POST response. `proxy.rs` is a real local gateway that replaces visitor
forwarding fields with fixed synthetic assertions. Neither needs AWS or DNS.

Run the ordinary subprocess tests with:

```sh
cargo test -p trusted-server-adapter-axum --target x86_64-unknown-linux-gnu \
  --test native_acceptance
```

The transport-sequence test aborts an inbound POST and also receives a truncated
origin POST response, then forwards three action-header POST/API pairs through
the same TS process and origin port. Its action origin is synthetic. This test
alone does not establish real Next.js server-action compatibility.

## Real Next.js regression

The existing integration Next.js fixture has no real server actions. Its unknown
ID scenario accepts 404/405, or a soft 404. That cannot prove that an action
executed after a truncated POST.

The `nextjs/app` files add a real server action that checks its input and
redirects to `/?action=accepted`. The ignored acceptance test extracts the
compiled form action ID from the proxied page, aborts an upload, submits that
real action and requires its 303 redirect, then requires exact API JSON. The
origin explicitly allows only `publisher.example.com` for server actions, and
the test sends that public Host and Origin. This is normal reverse-proxy origin
configuration, not disabled CSRF protection.

Prepare an isolated fixture with the existing integration fixture's lockfile.
Use the repository's pinned Node version. From the repository root:

```sh
NEXT_FIXTURE_ROOT=$(mktemp -d /tmp/ts-native-nextjs.XXXXXX)
cp crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package{,-lock}.json \
  "$NEXT_FIXTURE_ROOT/"
cp -R crates/trusted-server-adapter-axum/tests/fixtures/native/nextjs/app \
  "$NEXT_FIXTURE_ROOT/"
cp crates/trusted-server-adapter-axum/tests/fixtures/native/nextjs/next.config.mjs \
  "$NEXT_FIXTURE_ROOT/"
npm ci --prefix "$NEXT_FIXTURE_ROOT" --ignore-scripts
NATIVE_NEXTJS_PORT=$(node -e \
  'const s=require("node:net").createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
export NATIVE_NEXTJS_ORIGIN="http://127.0.0.1:$NATIVE_NEXTJS_PORT"
(
  cd "$NEXT_FIXTURE_ROOT"
  NEXT_TELEMETRY_DISABLED=1 timeout --signal=TERM --kill-after=3s 180s \
    node node_modules/next/dist/bin/next dev --hostname 127.0.0.1 \
    --port "$NATIVE_NEXTJS_PORT"
) >"$NEXT_FIXTURE_ROOT/next.log" 2>&1 &
NEXT_FIXTURE_PID=$!
```

Prewarm `/` and `/api/data` directly before the two-second acceptance request
budget. These commands must return 200 before starting the Rust test:

```sh
curl --fail --retry 20 --retry-connrefused --retry-delay 1 --max-time 60 \
  "$NATIVE_NEXTJS_ORIGIN/" >"$NEXT_FIXTURE_ROOT/page.html"
curl --fail --max-time 60 "$NATIVE_NEXTJS_ORIGIN/api/data"
cargo test -p trusted-server-adapter-axum --target x86_64-unknown-linux-gnu \
  --test native_acceptance real_nextjs_action_and_api_after_truncated_post \
  -- --ignored --exact --nocapture
```

The 180-second fixture limit includes compilation and the test. Start it again
if Cargo compilation consumed that budget. To clean up, allow `timeout` to drain
and kill the process group, then remove the temporary directory:

```sh
wait "$NEXT_FIXTURE_PID" || true
rm -rf -- "$NEXT_FIXTURE_ROOT"
```

This opt-in test fails if setup is absent. A default-suite ignored result is not
proof of the required real Next.js regression. It covers an aborted inbound
POST before a real progressively enhanced action/API sequence. The ordinary
synthetic test separately covers a truncated upstream POST response. Neither
claims to reproduce the historical failure until executed and compared.
