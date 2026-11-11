# Publisher attestation fixtures

These are synthetic protocol examples for the proposed publisher attestation
contract. They do not implement a production verifier, run a publisher endpoint,
or qualify a provider method.

Use the Node version in `.tool-versions`. Install the documentation dependencies
with `npm ci` in `docs/`, then run from the repository root:

```bash
node scripts/publisher-attestation/generate.mjs --check
node --test scripts/publisher-attestation/*.test.mjs
```

The decoded payloads, unsigned documents and cases in
`docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json` are the
declarative inputs. After changing them, run the generator without `--check`,
then format the JSON with the documentation Prettier configuration. The generator
reconstructs keys from fixed public **test-only** seeds and regenerates signatures,
thumbprints, binding/projection/policy hashes, challenge TXT values and embedded
transport objects. Never use these seeds or keys for real identities or data.

The fixture's `harness` member defines independent case contexts, RFC 7396
merge-patch overrides, selected inputs, numbered validation stages and policy
documents. The test runner evaluates the bounded cases using synthetic state;
it has no network, live DNS, provider collection, durable signer or production
trust configuration. Evidence-only success is a self-declaration. Consumer
endorsement requires matching a separately signed assessment to a fresh response.

Schema tests cover canonical DNS/HTTPS identifiers, provider fingerprint context,
environment invariants and every ordinary fixture object. Other tests check exact
signed bytes, derived hashes, raw duplicate-member encodings, strict point
admission and the declared acceptance/rejection cases. The CI documentation job
runs the same commands. These checks complement, rather than replace, independent
cryptographic interoperability and future runtime qualification.
