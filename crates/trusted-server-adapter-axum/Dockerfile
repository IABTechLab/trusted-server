# Trusted Server on Axum as a container image.
#
# Build from the repository root:
#   docker build --file crates/trusted-server-adapter-axum/Dockerfile \
#     --build-arg TRUSTED_SERVER_VERSION="$(git rev-parse HEAD)" \
#     --tag trusted-server-axum .
#
# The image is an ordinary build with build provenance only. It is not part of
# the reproducible release cohort that the binary release rebuild checks cover.
# Axum is development status; see docs/guide/axum-dev.md.

# The tsjs build script runs npm; this is the `.tool-versions` Node.
FROM docker.io/library/node:24.20.0-bookworm-slim@sha256:ba849c60be29959425b8734d57b8b4b7d56f98edd9504c9af091d5281095a71e AS node

# Debian 12 builder and runtime share glibc 2.36.
FROM docker.io/library/rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
RUN set -eu; \
    ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm; \
    ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx; \
    test "$(node --version)" = v24.20.0; \
    npm --version

# Use the installed toolchain as is: rust-toolchain.toml would otherwise make
# rustup download its extra wasm targets, an unpinned network input.
ENV RUSTUP_TOOLCHAIN=1.98.1

WORKDIR /src
COPY . .

# The checkout has no `.git`, so pass the source commit as the embedded
# version, as the release binaries do.
ARG TRUSTED_SERVER_VERSION
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    set -eu; \
    if [ -n "${TRUSTED_SERVER_VERSION:-}" ]; then \
      export TRUSTED_SERVER__GIT_VERSION="$TRUSTED_SERVER_VERSION"; \
    fi; \
    host="$(rustc -vV | sed -n 's/^host: //p')"; \
    cargo build --release --locked --package trusted-server-adapter-axum \
      --bin trusted-server-axum --target "$host"; \
    install -D -m 0755 "target/$host/release/trusted-server-axum" \
      /out/trusted-server-axum

# Debian slim with CA certificates only: no Node, compiler, config or secrets.
# Trusted Server's origin fetches use compiled-in webpki roots; only EdgeZero's
# own client reads this system store.
FROM docker.io/library/debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

RUN set -eu; \
    apt-get update; \
    apt-get install --yes --no-install-recommends ca-certificates; \
    rm -rf /var/lib/apt/lists/*

COPY --from=build /out/trusted-server-axum /usr/local/bin/trusted-server-axum

# Containers must accept connections from outside their own loopback. The
# process writes nothing, so it runs with a read-only root filesystem.
ENV EDGEZERO__ADAPTER__HOST=0.0.0.0 \
    EDGEZERO__ADAPTER__PORT=8080
EXPOSE 8080
# The server shuts down on SIGINT only, and a PID 1 without a SIGTERM handler
# would ignore `docker stop` until the kill timeout.
STOPSIGNAL SIGINT
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/trusted-server-axum"]
