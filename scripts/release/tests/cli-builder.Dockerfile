# Ordinary PR fixture only. This image does not qualify any release recipe.
# UBI 9 keeps glibc 2.34 for the whole RHEL 9 lifecycle, so the Linux `ts`
# links no symbol version above the reviewed ceiling. gcc (with glibc-devel)
# is rustc's linker driver and the C compiler of Cargo build scripts (bundled
# SQLite, ring, aws-lc-sys); git-core matches the optional `git` lookups the
# Fastly fixture image also offers.
FROM registry.access.redhat.com/ubi9/ubi:9.6
RUN dnf install -y --setopt=install_weak_deps=False --nodocs gcc glibc-devel git-core \
    && dnf clean all \
    && rm -rf /var/cache/dnf
