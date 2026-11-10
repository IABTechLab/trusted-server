# Trusted Server on Axum as a container image.
#
# Build from the repository root:
#   docker build --file crates/trusted-server-adapter-axum/Dockerfile \
#     --build-arg TRUSTED_SERVER_VERSION=v1.2.3 --tag trusted-server-axum .
#
# The image is an ordinary build with build provenance only. It is not part of
# the reproducible release cohort that the binary release rebuild checks cover.

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

WORKDIR /src
COPY . .

# The checkout has no `.git`, so the release tag names the embedded version.
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

# Distroless keeps glibc, CA certificates and a nonroot user, with no shell.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

COPY --from=build /out/trusted-server-axum /usr/local/bin/trusted-server-axum

# Containers must accept connections from outside their own loopback.
ENV TRUSTED_SERVER_LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/trusted-server-axum"]
