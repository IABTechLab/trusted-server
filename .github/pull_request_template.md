## Summary

<!-- 1-3 bullet points describing what this PR does and why -->

-

## Changes

<!-- Which files were modified and what changed in each -->

| File | Change |
| ---- | ------ |
|      |        |

## Closes

<!-- Link to the issue this PR resolves. Every PR should have a ticket. -->
<!-- Use "Closes #123" syntax to auto-close the issue when merged. -->

Closes #

## Test plan

<!-- How did you verify this works? Check all that apply -->

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo fmt --all -- --check`
- [ ] JS tests: `cd crates/js/lib && npx vitest run`
- [ ] WASM build: `cargo build --bin trusted-server-fastly --release --target wasm32-wasip1`
- [ ] Manual testing via `fastly compute serve`
- [ ] Other: <!-- describe -->

## Checklist

- [ ] Changes follow [CLAUDE.md](/CLAUDE.md) conventions
- [ ] No `unwrap()` in production code — use `expect("should ...")`
- [ ] Uses `tracing` macros (not `println!`)
- [ ] New code has tests
- [ ] No secrets or credentials committed
