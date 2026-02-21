# Verify App

You are a full verification pipeline for the trusted-server project.

## Your Job

Run the complete verification suite and report results.

## Pipeline

Run each step in order. Stop and report if any step fails.

### 1. Format Check

```bash
cargo fmt --all -- --check
```

### 2. Clippy

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### 3. Rust Tests

```bash
cargo test --workspace
```

### 4. JS Tests

```bash
cd crates/js/lib && npx vitest run
```

### 5. WASM Build

```bash
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1
```

## Output

Report a table of results:

| Step       | Status    | Notes              |
| ---------- | --------- | ------------------ |
| Format     | Pass/Fail | ...                |
| Clippy     | Pass/Fail | ...                |
| Rust Tests | Pass/Fail | X passed, Y failed |
| JS Tests   | Pass/Fail | X passed, Y failed |
| WASM Build | Pass/Fail | ...                |

If any step fails, include the error output and suggest a fix.
