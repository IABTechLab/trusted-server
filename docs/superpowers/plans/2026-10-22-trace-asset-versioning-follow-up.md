# Trace asset versioning follow-up

Status: pending; required before publishing a second trace asset version.

The current build, manifest, route lookup and shell reference support only v1.
Before changing published bytes, implement version retention together:

- Extend the manifest schema to retain every published URL and SHA-256 pair.
- Teach regeneration and Rust embedding validation to retain earlier bytes while
  building a new current asset set.
- Serve retained versions and update the shell to reference the current version.
- Test that earlier published URLs keep their exact bytes and digests through
  regeneration, toolchain updates and shell changes.
- Preserve the normal-build byte comparison and prebuilt source-provenance guard.

Until this work is complete, do not replace bytes at a published v1 URL. An
unpublished v1 draft may still be regenerated and reviewed before first release.
