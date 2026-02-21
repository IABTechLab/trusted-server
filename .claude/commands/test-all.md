Run the full test suite for both Rust and JavaScript.

```bash
cargo test --workspace
```

Then run JS tests:

```bash
cd crates/js/lib && npx vitest run
```

Report results for both. If any test fails, investigate and suggest a fix.
