# syntax=docker/dockerfile:1
# Both inputs must be explicit accepted digest references. Build at repository root.
ARG TS_NATIVE_PROVIDER_IMAGE
ARG TS_NATIVE_RUNTIME_IMAGE
FROM ${TS_NATIVE_PROVIDER_IMAGE} AS binary-source
FROM ${TS_NATIVE_RUNTIME_IMAGE} AS runtime

ARG TS_NATIVE_PROVIDER_IMAGE
ARG TS_NATIVE_RUNTIME_IMAGE
# No runtime operation needs setuid/setgid tools. Nonroot alone is insufficient.
RUN set -eu; \
    printf '%s\n' "$TS_NATIVE_PROVIDER_IMAGE" | grep -Eq '@sha256:[a-f0-9]{64}$'; \
    printf '%s\n' "$TS_NATIVE_RUNTIME_IMAGE" | grep -Eq '@sha256:[a-f0-9]{64}$'; \
    . /etc/os-release; test "$ID:$VERSION_ID" = debian:13; \
    apt-get update; \
    apt-get upgrade -y --no-install-recommends; \
    apt-get install -y --no-install-recommends ca-certificates libgcc-s1 coreutils; \
    rm -rf /var/lib/apt/lists/*; \
    find / -xdev -type f -perm /6000 -exec chmod a-s {} +; \
    install -d -m 0700 -o 10001 -g 10001 /run/trusted-server
LABEL ts.native.provider-image="${TS_NATIVE_PROVIDER_IMAGE}" \
      ts.native.runtime-image="${TS_NATIVE_RUNTIME_IMAGE}"
WORKDIR /app
COPY --from=binary-source --chmod=0555 /usr/local/bin/app /usr/local/bin/app
COPY --from=binary-source /usr/local/share/ts-build-inputs.sha256 /usr/local/share/ts-build-inputs.sha256
COPY --chmod=0555 deploy/runtime/native-aws/entrypoint.sh /usr/local/bin/ts-native-entrypoint
ENV EDGEZERO__ADAPTER__HOST=0.0.0.0 \
    EDGEZERO__ADAPTER__PORT=8787 \
    EDGEZERO__STORE_BINDINGS_FILE=/run/trusted-server/native-bindings.toml
USER 10001:10001
# ECS must still qualify ownership on its matching task volume mount.
VOLUME ["/run/trusted-server"]
EXPOSE 8787
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/ts-native-entrypoint"]
