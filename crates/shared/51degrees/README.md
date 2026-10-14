# What the 51Degrees modules share

`trusted-server-shared-51degrees` holds what more than one 51Degrees module
needs, so each module's crate under `crates/<type>/51degrees` depends on this
one and on none of the others, and none of it is in core.

- `config`: the vendor's table, `[shared.51degrees]`, being the cloud
  endpoint, the license key and the timeout. Its presence runs the 51Degrees
  builder.
- `license`: the license key every server side call presents, and the
  refusal when the table has none.
- `client`: the one call to the cloud service for a request's evidence and
  the answer it gives, held in the loading cache so a question many requests
  ask at once is asked once.
- `evidence`: the evidence a request gives the call, being the address, the
  `User-Agent` and what the browser gathered into its `51D_` cookies.
- `engine_cache`: the cache the answers are held in, `51degrees-answers`,
  selected and sized in `[cache.51degrees-answers]`.
- `hints_engine`: the client hints question, which asks the service which
  hints it wants from a browser.
- `test_services`: runtime services for the 51Degrees crates' tests, behind
  the `test-utils` feature.

The modules that use it are documented in the
[51Degrees guide](../../../docs/guide/integrations/51degrees.md).
