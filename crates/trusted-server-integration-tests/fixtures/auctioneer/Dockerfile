# Build from a clean git archive, with a qualified Linux Rust/Node build image.
# No Docker socket is present in this worker image or its runtime container.
ARG BUILDER_IMAGE
FROM ${BUILDER_IMAGE} AS builder
USER root
ARG HARNESS_COMMIT
WORKDIR /source
COPY . .
ENV PATH=/opt/node/bin:/opt/rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu/bin:/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin
ENV TRUSTED_SERVER__GIT_VERSION=${HARNESS_COMMIT}
ENV CARGO_TARGET_DIR=/worker-target
RUN test "$(node --version)" = v24.20.0 && test "$(rustc --version | cut -d ' ' -f 2)" = 1.98.1
RUN npm --prefix crates/trusted-server-js/lib ci && npm --prefix crates/trusted-server-integration-tests/browser ci
RUN node crates/trusted-server-js/lib/build-prebid-external.mjs --modules-json '{"bidder":["rubiconBidAdapter"],"userId":[],"analytics":[]}' --out /opt/prebid
RUN cargo test --locked --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime --no-run --message-format=json > /tmp/worker-build.jsonl
RUN node -e 'const fs=require("fs");const rows=fs.readFileSync("/tmp/worker-build.jsonl","utf8").trim().split("\n").map(JSON.parse);const bins=rows.filter(x=>x.reason==="compiler-artifact"&&x.target.name==="html_eof_runtime"&&x.executable);if(bins.length!==1)throw Error("Expected one native worker");fs.copyFileSync(bins[0].executable,"/opt/production-worker");fs.chmodSync("/opt/production-worker",0o755)'
RUN rm -rf crates/trusted-server-js/lib/node_modules
RUN curl -fsSL https://github.com/fastly/Viceroy/releases/download/v0.22.0/viceroy_v0.22.0_linux-amd64.tar.gz -o /tmp/viceroy.tar.gz && echo '03f66f27275e8adc0c1857c4f571bd6f5ae997e38cc43760b19c6840a92b298d  /tmp/viceroy.tar.gz' | sha256sum -c - && tar -xzf /tmp/viceroy.tar.gz -C /opt viceroy

FROM mcr.microsoft.com/playwright:v1.58.2-noble@sha256:65cefd09a5e943921ecd3a6e5414c603db2eb161e9eb48f2e2ccc63486dc7dc0
USER root
ARG HARNESS_COMMIT
LABEL org.opencontainers.image.revision=${HARNESS_COMMIT}
RUN apt-get update && apt-get install -y --no-install-recommends libnss3-tools openssl binutils && dpkg-query -W > /opt/dpkg-packages.txt && rm -rf /var/lib/apt/lists/*
COPY --from=builder /opt/node /opt/node
COPY --from=builder /opt/viceroy /usr/local/bin/viceroy
COPY --from=builder /opt/production-worker /opt/production-worker
COPY --from=builder /source /source
COPY --from=builder /opt/prebid /opt/prebid
ENV PATH=/opt/node/bin:/usr/local/bin:/usr/bin:/bin
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
RUN mkdir /inputs /work /results && ln -s /opt/prebid /inputs/prebid && ldd /opt/production-worker > /opt/worker-ldd.txt && ! grep -q 'not found' /opt/worker-ldd.txt && readelf --version-info /opt/production-worker > /opt/worker-elf.txt && /opt/production-worker --list > /opt/worker-tests.txt && grep -q 'production_pbs_worker: test' /opt/worker-tests.txt && node --version && viceroy --version
ENTRYPOINT ["/opt/production-worker", "--exact", "production_pbs_worker", "--ignored", "--nocapture"]
