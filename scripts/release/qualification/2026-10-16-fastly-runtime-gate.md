# Task 9: Fastly final-byte runtime gate

Status: on 2026-10-16 ordinary PR CI ran the Fastly final-byte gate on both
the canonical and the reproduction `trusted-server-fastly.wasm` outputs, and
both were accepted. The same run rejected a closed-origin control as a
health-only success. This is candidate evidence from an ordinary CI runner and
records `release_qualification: false`. Viceroy is pinned by version text and
recorded by digest, not by an independently reviewed digest; that pin, the
toolchain, the image and the acquired inputs remain unqualified.

## Run

The job
[`cargo test (confined release Cargo candidate, Linux)`](https://github.com/IABTechLab/trusted-server/actions/runs/38875569290/job/116889681434)
checked out the pull request merge commit
`ab6cba207192969c55547876f9adf7d625a64ea1` (branch head
`4cc5700e834c3238f99a81b3a567edb634fecfe1`). The ignored test
`recipe::tests::full::fastly_canonical_and_reproduction_builds_are_byte_identical`
passed in 1,113.82 seconds. It built both roles from one JS handoff (confined
builds of 132 and 166 seconds), found them byte-identical (66,833,956 bytes,
SHA-256 `0c21ec136d2ffabf2d3440142d741e96b2aef18d5a52c82d4cfc8022dc0866c9`),
then ran the gate on each output without rebuilding. The denial-inheritance
test passed again in the same job.

## What the gate checks

1. The exact final bytes are re-read against the recorded size and digest and
   fully validated as a core Wasm command with the pinned wasmparser: no
   component, only Fastly import modules, a no-argument `_start`, an exported
   memory, the HTTP request and response modules, and debug sections.
2. The verified bytes are copied read-only into a private directory and served
   by the explicitly selected Viceroy, whose `--version` output must be exactly
   `viceroy 0.17.0` and whose binary is rehashed around every use. Unknown
   imports fail to link. The configuration comes from the integration
   harness's fixtures, which must match the built source.
3. `/health` only gates readiness. Acceptance requires a 200 HTML publisher
   page that carries this run's unique origin marker once and exactly one
   injected tsjs script, an origin request observed by the gate's loopback
   origin, no guest failure in Viceroy's output, and a stop initiated by the
   gate. The artifact rehashes unchanged afterwards. Responses are parsed
   strictly over loopback only.

## Results

| Output       | Health | Publisher | Injected script                                                                                       | Origin request   | Viceroy stderr | Startup |
| ------------ | ------ | --------- | ----------------------------------------------------------------------------------------------------- | ---------------- | -------------- | ------- |
| Canonical    | 200    | 200       | `/static/tsjs=tsjs-unified.min.js?v=662e376b054ac1fccc82a9c8f0d2a086bfa11541343cb0af8c046ef09ac5aa75` | `GET / HTTP/1.1` | empty          | 4.26 s  |
| Reproduction | 200    | 200       | same                                                                                                  | `GET / HTTP/1.1` | empty          | 4.26 s  |

Viceroy 0.17.0 binary SHA-256:
`05da4843fd8ae870409211e9fbc168aafbb1609445fbd0eefa3902e5a151cce1`. The
closed-origin control was rejected with "publisher page status is not 200" and
tagged as a health-only success.

## Not covered

The three `ts` executables, producer-owned Git manifest normalization, the
production reproduction profile, an independently reviewed Viceroy digest,
authenticated acquisition, qualified pins and hosted qualification remain open
under Tasks 8 and 9. Live deployment is separate.
