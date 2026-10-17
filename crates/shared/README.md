# Shared vendor crates

What a vendor offers to every crate that wants it lives here, one crate per
vendor at `crates/shared/<vendor>`, as a vendor's own modules live under the
type folders beside it. A crate here carries no module a section selects. It
offers what it has on its builder, and core calls it through the interface
core declares for that thing.

- `crates/shared/aws` (`trusted-server-shared-aws`) offers the origin signer
  `s3_sigv4`, which a source's `auth` table names to sign a read from S3 with
  AWS Signature Version 4. Core's `origin_auth` module declares the signer
  interface, the `OriginSigner` trait, and each adapter lists the crate's
  builder among the modules it links.

A vendor whose crates share settings, such as an endpoint and a license key,
writes them in one `[shared.<vendor>]` table, which its builder reads with
`.with_shared_settings(vendor)`. The table's name is the vendor's folder name
here.
