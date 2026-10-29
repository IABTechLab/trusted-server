#!/usr/bin/env bash
# Exercise generator freshness and executable overrides without compiling Rust.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE=$(mktemp -d)
trap 'rm -rf -- "$WORKSPACE"' EXIT

mkdir -p "$WORKSPACE/bin"
cat > "$WORKSPACE/bin/rustc" <<'SH'
#!/usr/bin/env bash
printf 'host: fixture-host\n'
SH
cat > "$WORKSPACE/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'build\n' >> "$GENERATOR_TEST_CARGO_LOG"
cp "$GENERATOR_TEST_CURRENT" "$GENERATOR_TEST_DEFAULT"
SH
cat > "$WORKSPACE/current-generator" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
while [ "$#" -gt 0 ]; do
    if [ "$1" = --output ]; then
        output="$2"
        break
    fi
    shift
done
printf '%s\n' '[
  {"descriptor_id":"handler.password","native":{"native_name":"fixture_handler"},"value":"fictional-handler"},
  {"descriptor_id":"publisher.proxy","native":{"native_name":"fixture_proxy"},"value":"fictional-proxy"},
  {"descriptor_id":"ec.passphrase","native":{"native_name":"fixture_ec"},"value":"fictional-passphrase"}
]' > "$output"
SH
chmod +x "$WORKSPACE/bin/"* "$WORKSPACE/current-generator"

run_case() (
    local script="$1"
    local state="$2"
    local repository="$WORKSPACE/$script-$state"
    local workspace="$repository/workspace"
    local generator="$repository/crates/trusted-server-integration-tests/target/fixture-host/debug/generate-viceroy-config"
    mkdir -p "$repository/scripts" "$(dirname "$generator")" "$workspace" \
        "$repository/crates/trusted-server-adapter-spin" \
        "$repository/crates/trusted-server-adapter-cloudflare"
    cp "$REPO_ROOT/scripts/generate-integration-viceroy-configs.sh" "$repository/scripts/"
    export PATH="$WORKSPACE/bin:$PATH"
    export GENERATOR_TEST_CARGO_LOG="$repository/cargo.log"
    export GENERATOR_TEST_CURRENT="$WORKSPACE/current-generator"
    export GENERATOR_TEST_DEFAULT="$generator"
    unset GENERATOR_BIN

    case "$state" in
        cached)
            # A stale default must be replaced by Cargo before it can run.
            printf '#!/usr/bin/env bash\nexit 89\n' > "$generator"
            chmod +x "$generator"
            ;;
        missing) ;;
        override)
            export GENERATOR_BIN="$repository/custom-generator"
            cp "$WORKSPACE/current-generator" "$GENERATOR_BIN"
            ;;
    esac

    if [ "$script" = generate ]; then
        bash "$repository/scripts/generate-integration-viceroy-configs.sh" --local-manifests
    else
        source "$REPO_ROOT/scripts/smoke-common.sh"
        SMOKE_APP_CONFIG="$workspace/trusted-server.toml"
        smoke_generate_bindings "$repository" "$workspace" axum
        [ "$SMOKE_HANDLER_ENV" = fixture_handler ]
        [ "$SMOKE_PROXY_ENV" = fixture_proxy ]
        [ "$SMOKE_EC_ENV" = fixture_ec ]
    fi

    if [ "$state" = override ]; then
        [ ! -e "$GENERATOR_TEST_CARGO_LOG" ]
    else
        [ "$(wc -l < "$GENERATOR_TEST_CARGO_LOG")" -eq 1 ]
    fi
    printf 'PASS: %s %s\n' "$script" "$state"
)

for script in generate smoke; do
    for state in cached missing override; do
        run_case "$script" "$state"
    done
done
