# Ordinary PR fixture only. This image does not qualify any release recipe.
FROM ubuntu:24.04
RUN apt-get update \
    && apt-get install -y --no-install-recommends git libstdc++6 libgcc-s1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
