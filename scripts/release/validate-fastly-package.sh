#!/usr/bin/env bash
# Validate the generated release package and the manual fallback without deployment.
set -euxo pipefail
fastly_version=$(awk '$1 == "fastly" { print $2 }' .tool-versions)
test "$fastly_version" = 16.0.0
route=$(mktemp -d "$RUNNER_TEMP/fastly-route.XXXXXX")
mkdir "$route/tool" "$route/config" "$route/cache" "$route/tmp" "$route/package"
archive="fastly_v${fastly_version}_linux-amd64.tar.gz"
curl --fail --silent --show-error --location --proto '=https' --retry 2 --max-time 30 \
  "https://github.com/fastly/cli/releases/download/v${fastly_version}/$archive" --output "$route/$archive"
printf '%s  %s\n' b1b89112f1f1291ff275393779ffdacba8724ac64091bda0fa6000ae06308459 "$archive" > "$route/CLI.sha256"
(cd "$route" && sha256sum --check CLI.sha256)
tar -xzf "$route/$archive" -C "$route/tool"
cp release-assets/trusted-server-fastly.wasm "$route/package/trusted-server-fastly.wasm"
cp release-assets/trusted-server-fastly.tar.gz "$route/package/trusted-server-fastly.tar.gz"
cp scripts/release/fastly.toml "$route/package/release-fastly.toml"
printf 'manifest_version = 3\nname = "example-app"\n' > "$route/package/fastly.toml"
expected_wasm=$(sha256sum release-assets/trusted-server-fastly.wasm | awk '{print $1}')
expected_manifest=$(sha256sum "$route/package/fastly.toml" | awk '{print $1}')
producer="$RUNNER_TEMP/ordinary-producer-target/x86_64-unknown-linux-gnu/release/trusted-server-release"
node_program=$(command -v node)
host_network=$(readlink /proc/self/ns/net)
sudo unshare --net -- setpriv --reuid="$(id -u)" --regid="$(id -g)" \
  --clear-groups --inh-caps=-all --ambient-caps=-all --bounding-set=-all --no-new-privs \
  env -i PATH="$route/tool:/usr/bin:/bin:/usr/sbin:/sbin" XDG_CONFIG_HOME="$route/config" \
  XDG_CACHE_HOME="$route/cache" TMPDIR="$route/tmp" LC_ALL=C TZ=UTC \
  bash -euxo pipefail -s -- "$route/package" "$producer" "$expected_wasm" "$expected_manifest" "$host_network" "$node_program" <<'ROUTE'
guest_network=$(readlink /proc/self/ns/net)
test -n "$guest_network"
test "$guest_network" != "$5"
active_interfaces=$(ip -o link show up)
test -z "$active_interfaces"
cd "$1"
timeout 15 fastly version | tee cli-version.txt
grep -E '^Fastly CLI version v16\.0\.0( |$)' cli-version.txt
timeout 30 fastly compute validate --package ./trusted-server-fastly.tar.gz
timeout 30 "$2" inspect-fastly-package --package-dir . --manifest ./release-fastly.toml \
  --expected-manifest-sha256 "$(sha256sum release-fastly.toml | awk '{print $1}')" --expected-wasm-sha256 "$3"
timeout 60 fastly compute pack --wasm-binary ./trusted-server-fastly.wasm
timeout 30 fastly compute validate --package ./pkg/example-app.tar.gz
tar -tzf ./pkg/example-app.tar.gz
timeout 15 "$6" --input-type=commonjs <<'HEADERS'
const fs = require('node:fs'), zlib = require('node:zlib');
const file = './pkg/example-app.tar.gz', stat = fs.lstatSync(file);
if (!stat.isFile() || stat.size > 768 * 1024 * 1024) throw Error('Diagnostic archive bound exceeded');
const bytes = zlib.gunzipSync(fs.readFileSync(file), {maxOutputLength:513 * 1024 * 1024 + 16384});
let offset = 0;
for (let count = 0; count < 8 && offset + 512 <= bytes.length; count++) {
  const header = bytes.subarray(offset, offset + 512);
  if (header.every(byte => byte === 0)) break;
  const field = (start, end) => header.subarray(start, end).toString('utf8').split('\0')[0];
  const hidden = (start, end) => { const data = header.subarray(start, end), zero = data.indexOf(0); return zero >= 0 && data.subarray(zero + 1).some(byte => byte !== 0); };
  const rawSize = field(124, 136).trim();
  if (!/^[0-7]+$/.test(rawSize)) throw Error('Unsupported diagnostic size encoding');
  const size = parseInt(rawSize, 8);
  if (!Number.isSafeInteger(size) || size > bytes.length) throw Error('Diagnostic member bound exceeded');
  console.log('Fastly raw TAR header:', JSON.stringify({path:field(0, 100), type:String.fromCharCode(header[156]), size, prefix:field(345, 500), link:field(157, 257), nonzero_padding:[[0,100],[157,257],[345,500]].some(([start,end]) => hidden(start,end))}));
  offset += 512 + Math.ceil(size / 512) * 512;
}
HEADERS
timeout 30 "$2" inspect-fastly-package --package-dir ./pkg --manifest ./fastly.toml \
  --expected-manifest-sha256 "$4" --expected-wasm-sha256 "$3"
tar -xOzf ./pkg/example-app.tar.gz example-app/bin/main.wasm > packaged.wasm
cmp trusted-server-fastly.wasm packaged.wasm
tar -xOzf ./pkg/example-app.tar.gz example-app/fastly.toml > packaged.toml
cmp fastly.toml packaged.toml
printf '%s  %s\n' "$3" trusted-server-fastly.wasm > input.sha256
sha256sum --check input.sha256
echo 'SUCCESS: pinned Fastly route packed, validated, inspected and byte-compared without credentials or network'
ROUTE
printf '%s  %s\n' "$expected_wasm" release-assets/trusted-server-fastly.wasm | sha256sum --check
