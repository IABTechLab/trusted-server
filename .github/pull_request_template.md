## Summary

<!-- 1-3 bullet points describing what this PR does and why -->

-

## Changes

<!-- Which files were modified and what changed in each -->

| File | Change |
| ---- | ------ |
|      |        |

## Closes

<!-- Link the issue this PR resolves. Features and other substantial changes need one; smaller PRs link one if it exists. -->
<!-- Use "Closes #123" syntax to auto-close the issue when merged. -->

Closes #

## Spec

<!-- Feature PRs need a spec, written with the superpowers brainstorming skill and reviewed with an AI coding agent over at least two rounds. -->
<!-- Link it here. For a bug fix, docs, dependency, CI, or behavior-preserving refactor, write "Not a feature" instead. When in doubt, write a spec. -->
<!-- Feature PRs without a spec go back to draft. Details: https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md#features-start-with-a-spec -->

Spec:

## Test plan

<!-- How did you verify this works? Check all that apply -->

- [ ] `cargo test-fastly && cargo test-axum`
- [ ] `cargo clippy-fastly && cargo clippy-axum`
- [ ] `cargo fmt --all -- --check`
- [ ] JS tests: `cd crates/trusted-server-js/lib && npx vitest run`
- [ ] JS format: `cd crates/trusted-server-js/lib && npm run format`
- [ ] Docs format: `cd docs && npm run format`
- [ ] WASM build: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
- [ ] Manual testing via `fastly compute serve`
- [ ] Other: <!-- describe -->

## Checklist

- [ ] Feature PRs: spec linked above, ending in a review log of two or more agent review rounds
- [ ] Changes follow [AGENTS.md](/AGENTS.md) conventions
- [ ] No `unwrap()` in production code — use `expect("should ...")`
- [ ] Uses `log` macros (not `println!`)
- [ ] New code has tests
- [ ] No secrets or credentials committed
