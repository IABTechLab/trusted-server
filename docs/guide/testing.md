# Testing

Learn how to test Trusted Server locally and in CI/CD.

## Test Infrastructure

### Viceroy

Viceroy is the local test runtime for Fastly Compute applications.

```bash
# Install viceroy
cargo install viceroy

# Run tests
cargo test
```

### Test Organization

Tests are organized alongside source code in `#[cfg(test)]` modules:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_synthetic_id_generation() {
        // Test implementation
    }
}
```

## Running Tests

### Unit Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run tests for specific crate
cargo test -p trusted-server-common
```

### Integration Tests

```bash
# Run integration tests
cargo test --test '*'
```

### Local Server Testing

```bash
# Start local server
fastly compute serve

# Test with curl
curl http://localhost:7676/health
```

## Test Categories

### Synthetic ID Tests

Test ID generation, validation, and rotation:

```rust
#[test]
fn test_id_generation_with_consent() {
    // Placeholder test
}
```

### GDPR Compliance Tests

Test consent validation and enforcement:

```rust
#[test]
fn test_reject_without_consent() {
    // Placeholder test
}
```

### Ad Serving Tests

Test ad server integration and creative processing:

```rust
#[test]
fn test_ad_request_flow() {
    // Placeholder test
}
```

## Mock Data

Test fixtures located in:
- `tests/fixtures/` - Test data files
- `tests/kv_store/` - Mock KV store data

## CI/CD Testing

### GitHub Actions

Tests run automatically on:
- Pull requests
- Main branch commits
- Release tags

```yaml
# .github/workflows/test.yml
name: Test
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Run tests
        run: cargo test
```

## Code Quality

### Formatting

```bash
# Check formatting
cargo fmt --check

# Auto-format code
cargo fmt
```

### Linting

```bash
# Run clippy
cargo clippy --all-targets --all-features

# Fix clippy warnings
cargo clippy --fix
```

### Code Coverage

```bash
# Generate coverage report (requires cargo-tarpaulin)
cargo tarpaulin --out Html
```

## Performance Testing

### Load Testing

Use tools like:
- `wrk` - HTTP benchmarking
- `hey` - Load generator
- `k6` - Modern load testing

```bash
# Example with wrk
wrk -t12 -c400 -d30s http://localhost:7676/
```

## Debugging Tests

### Enable Debug Logging

```bash
RUST_LOG=debug cargo test -- --nocapture
```

### Running Single Tests

```bash
cargo test test_name -- --nocapture --test-threads=1
```

## Best Practices

1. Write tests for all new features
2. Maintain high test coverage
3. Use descriptive test names
4. Test edge cases and error conditions
5. Keep tests fast and isolated
6. Mock external dependencies

## Continuous Integration

All tests must pass before:
- Merging pull requests
- Deploying to staging
- Releasing to production

## Next Steps

- Review [Architecture](/guide/architecture)
- Configure your [Deployment](/guide/configuration)
