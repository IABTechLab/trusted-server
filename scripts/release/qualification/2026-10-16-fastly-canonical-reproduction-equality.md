# Task 9: Fastly canonical and reproduction builds are byte-identical

Status: on 2026-10-16 ordinary PR CI built `trusted-server-fastly.wasm` twice
through `PreparedRecipe::execute`, as the canonical build (`Role::Canonical`,
the spec's Build A) and as the reproduction build (`Role::Reproduction`, Build
B), from one JS handoff and in separate fresh roots and denial boundaries. The
final bytes were identical, and a scan of those bytes proved that the path
remapping took effect. This is candidate evidence from an ordinary CI runner
and records `release_qualification: false`. The reproduction profile comes from
a test-only alternate; the production reproduction profile, the toolchain, the
image, the acquired crates and the `cargo vendor` Git manifests remain
unqualified.

## Runs

| Run                                                                                                                      | Source (PR merge commit)                   | Final SHA-256                                                      | Size (bytes) | Test duration |
| ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------ | ------------------------------------------------------------------ | ------------ | ------------- |
| [Primary, after the role rename](https://github.com/IABTechLab/trusted-server/actions/runs/38856856376/job/116830159922) | `4243e3a1c16bf1212ab585eecce8592bd5137950` | `a17a77cc5119384d2fb5f74d83cb76cbf264c768e508b8545cf8537db481f178` | 66,833,859   | 1,119.36 s    |
| [Earlier, before the rename](https://github.com/IABTechLab/trusted-server/actions/runs/38855948935/job/116827324356)     | `373463048c22237ba6e045e7fd0706bb4a104568` | `e5390f47e025f34b70190c944134d6e2b7209e792e8a4ec2d64c65ead594ca4b` | 66,833,943   | 1,064.12 s    |

Each run built both roles from the same commit and matched. The two runs
differ from each other because the source commit is embedded in the binary.
The earlier run's measurement still labels the roles `A` and `B`. Test
durations include the full two-fresh-installation JS acceptance (1,275 tests
per side, 13 byte-identical modules) that produces the shared JS handoff. Both
runs also repeated the build-script, grandchild and procedural-macro denial
test.

## What varied

| Dimension        | Canonical                       | Reproduction                    |
| ---------------- | ------------------------------- | ------------------------------- |
| umask            | 022                             | 027                             |
| Cargo jobs       | 4                               | 2                               |
| Host checkout    | separate fresh temporary copy   | separate fresh temporary copy   |
| Start time       | recorded per run                | recorded per run                |
| Locale, timezone | normalized to `C`, `UTC`        | normalized to `C`, `UTC`        |
| Execution user   | same CI user (unsupported here) | same CI user (unsupported here) |

In the primary run the confined builds took 137 and 171 seconds. Inside the
Linux container both roles see every root at the same `/boundary/<kind>/<index>`
path, so the checkout-root difference is a host-only variation there.

## Remapping proof

The build remaps the checkout, vendor tree, Cargo home, Rust tool root and
target directory to fixed prefixes through one quoted
`--config target."<triple>".rustflags` entry, which Cargo appends to tracked
target rustflags. Because container paths match across roles on Linux, byte
equality alone would not prove the remapping. The test therefore scanned the
compared final bytes: they contained none of `/boundary/`, the host build
roots, the fixture root or `RUNNER_TEMP`, and they did contain the remapped
`/trusted-server` and `/cargo-vendor` prefixes.

## Not covered

The three `ts` executables, Viceroy final-byte acceptance, producer-owned Git
manifest normalization, the production reproduction profile, authenticated
acquisition, qualified tool and image pins, and hosted qualification remain
open under Tasks 8 and 9.
