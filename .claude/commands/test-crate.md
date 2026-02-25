Test a specific crate by name.

Usage: /test-crate $ARGUMENTS

Run:

```bash
cargo test -p $ARGUMENTS
```

If $ARGUMENTS is "js" or "javascript", run:

```bash
cd crates/js/lib && npx vitest run
```

Report results and investigate any failures.
