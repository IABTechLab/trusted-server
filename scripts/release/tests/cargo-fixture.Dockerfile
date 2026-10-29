# Ordinary PR fixture only. This image does not qualify any release recipe.
# gcc and libc6-dev give Cargo build scripts and proc macros a host linker.
FROM ubuntu:24.04
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gcc git libc6-dev libgcc-s1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
