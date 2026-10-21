# syntax=docker/dockerfile:1
# Adapted from EdgeZero 61bfe3b's hardened native recipe. Build at workspace root.
FROM node:24.12.0-bookworm-slim@sha256:7326fb2dbdce998edd72140946851be64ef4a643e8715e138ca467e8e9d92c99 AS node
FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS builder
COPY --from=node /usr/local/ /usr/local/
WORKDIR /src
COPY . .
# AWS providers are default-off. Direct Cargo builds must forward them explicitly.
ARG NATIVE_FEATURES=""
# Optional existing build metadata input; .git never enters the build context.
ARG TRUSTED_SERVER__GIT_VERSION=""
RUN <<'SH'
set -eu
test "$(rustc --version | awk '{print $2}')" = 1.98.1
test "$(node --version)" = v24.12.0
test "$(awk '$1 == "rust" {print $2}' .tool-versions)" = 1.98.1
grep -Eq '^channel = "1.98.1"$' rust-toolchain.toml
case "$NATIVE_FEATURES" in
  ""|aws-appconfig-agent|aws-secrets-manager|aws-appconfig-agent,aws-secrets-manager|aws-secrets-manager,aws-appconfig-agent) ;;
  *) echo 'Only the two declared native AWS provider features are supported' >&2; exit 1 ;;
esac
apt-get update
apt-get install -y --no-install-recommends cmake
rm -rf /var/lib/apt/lists/*
npm --prefix crates/trusted-server-js/lib ci --no-audit --no-fund
# No host target, node_modules or prebuilt dist enters this context. build.rs
# builds and validates new bundles in Cargo's private OUT_DIR before embedding.
host=$(rustc -vV | awk '$1 == "host:" {print $2}')
set --
if test -n "$NATIVE_FEATURES"; then set -- --features "$NATIVE_FEATURES"; fi
cargo build -p trusted-server-adapter-axum --bin trusted-server-axum --release --locked --target "$host" "$@"
mkdir /out
cp "target/$host/release/trusted-server-axum" /out/app
strip --strip-debug /out/app
SH

# Retain an auditable hash list of the exact context, not host working-tree state.
RUN find . -type d \( -name target -o -name node_modules \) -prune -o -type f ! -name .tsjs-npm-ci.lock -print0 \
    | sort -z | xargs -0 sha256sum > /out/build-inputs.sha256

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /out/app /usr/local/bin/app
COPY --from=builder /out/build-inputs.sha256 /usr/local/share/ts-build-inputs.sha256
ENV EDGEZERO__ADAPTER__HOST=0.0.0.0 \
    EDGEZERO__ADAPTER__PORT=8787 \
    EDGEZERO__ADAPTER__CONFIG_DIR=/config \
    EDGEZERO__ADAPTER__DATA_DIR=/data
USER 10001:10001
EXPOSE 8787
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/app"]
