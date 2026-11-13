# Publisher attestation fixtures

These are synthetic protocol examples for the proposed publisher attestation
contract. They do not implement a production verifier, run a publisher endpoint,
or qualify a provider method.

Use the Node version in `.tool-versions`. Install the documentation dependencies
with `npm ci` in `docs/`, then run from the repository root:

```bash
node scripts/publisher-attestation/generate.mjs --check
node --test scripts/publisher-attestation/*.test.mjs
docs/node_modules/.bin/eslint --config scripts/publisher-attestation/eslint.config.mjs "scripts/publisher-attestation/*.mjs"
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "scripts/publisher-attestation/*.mjs"
```

The decoded payloads, unsigned documents and cases in
`docs/superpowers/specs/fixtures/publisher-attestation-v1-vectors.json` are the
declarative inputs. After changing them, run the generator without `--check`,
which also formats the output with the documentation Prettier configuration. The generator
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

Normative policy documents live in `docs/superpowers/specs/policies/`; changing a
fixture copy does not change a policy. The synthetic official-shaped policy
qualifies no real provider method. Fixture member ordering remains input-preserved;
`--check` checks derivation and formatting, not a canonical top-level member order.
The node tests independently compare signature segments (including S) and the
enrollment TXT derivation, so corrupting those fields is detected without relying
on regeneration. The noncanonical R example necessarily overlaps small-order R;
a deliberately relaxed equation test proves it is not an arbitrary bad signature.

Every signed case supplies its expected message type. Trusted key material and
validity come only from independent case context. Status cases verify exact signed
bytes and target identifiers. Known withdrawal/compromise is modeled explicitly
in expiry-only mode. The harness has one issuer candidate: threshold 1 can pass,
and thresholds above 1 fail. Multi-issuer aggregation and a structured consumer
scope output remain runtime-library acceptance obligations, not tested outputs
of this bounded runner. Publisher membership cases simulate verifier-held account
and site authority; they do not perform DNS proof or deploy a registry.
