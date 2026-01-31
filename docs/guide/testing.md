# Testing

Learn how to test Trusted Server locally and in CI/CD.

## Test Infrastructure

### Viceroy

Viceroy is the local test runtime for Fastly Compute applications. It simulates the Fastly environment locally.

```bash
# Install viceroy
cargo install viceroy

# Run tests (viceroy is invoked automatically)
cargo test
```

### Test Organization

Tests are organized alongside source code in `#[cfg(test)]` modules:

```rust
// crates/common/src/synthetic.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_synthetic_id() {
        let settings = create_test_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::COOKIE, "pub_userid=12345"),
        ]);

        let synthetic_id = generate_synthetic_id(&settings, &req)
            .expect("should generate synthetic ID");

        assert!(!synthetic_id.is_empty());
    }
}
```

## Running Tests

### Unit Tests

```bash
# Run all tests
cargo test

# Run specific test by name
cargo test test_generate_synthetic_id

# Run tests with output visible
cargo test -- --nocapture

# Run tests for specific crate
cargo test -p trusted-server-common

# Run tests matching a pattern
cargo test synthetic
```

### Integration Tests

```bash
# Run all integration tests
cargo test --test '*'

# Run with single thread (useful for debugging)
cargo test -- --test-threads=1
```

### Local Server Testing

```bash
# Start local server
fastly compute serve

# Test endpoints with curl
curl http://localhost:7676/health
curl http://localhost:7676/.well-known/trusted-server.json
```

## Real Test Examples

### Synthetic ID Tests

From `crates/common/src/synthetic.rs`:

```rust
#[test]
fn test_get_synthetic_id_with_header() {
    let settings = create_test_settings();
    let req = create_test_request(vec![(
        HEADER_X_SYNTHETIC_ID,
        "existing_synthetic_id",
    )]);

    let synthetic_id = get_synthetic_id(&req)
        .expect("should get synthetic ID");
    assert_eq!(synthetic_id, Some("existing_synthetic_id".to_string()));
}

#[test]
fn test_get_synthetic_id_with_cookie() {
    let settings = create_test_settings();
    let req = create_test_request(vec![
        (header::COOKIE, "synthetic_id=existing_cookie_id")
    ]);

    let synthetic_id = get_synthetic_id(&req)
        .expect("should get synthetic ID");
    assert_eq!(synthetic_id, Some("existing_cookie_id".to_string()));
}

#[test]
fn test_get_synthetic_id_none() {
    let req = create_test_request(vec![]);
    let synthetic_id = get_synthetic_id(&req)
        .expect("should handle missing ID");
    assert!(synthetic_id.is_none());
}
```

### Creative Rewriting Tests

From `crates/common/src/creative.rs`:

```rust
#[test]
fn rewrites_width_height_attrs() {
    let settings = create_test_settings();
    let html = r#"<div><img width="1" height="1" src="https://t.example/p.gif"></div>"#;

    let out = rewrite_creative_html(&settings, html);

    assert!(out.contains("/first-party/proxy?tsurl="), "{}", out);
}

#[test]
fn injects_tsjs_creative_when_body_present() {
    let settings = create_test_settings();
    let html = r#"<html><body><p>hello</p></body></html>"#;

    let out = rewrite_creative_html(&settings, html);

    assert!(
        out.contains("/static/tsjs=tsjs-unified.min.js"),
        "expected unified tsjs injection: {}",
        out
    );
    // Inject only once
    assert_eq!(out.matches("/static/tsjs=tsjs-unified.min.js").count(), 1);
}

#[test]
fn rewrite_style_urls_handles_absolute_and_relative() {
    let settings = create_test_settings();
    let css = "background:url(https://cdn.example/a.png); border-image: url(/local/border.png)";

    let out = rewrite_style_urls(&settings, css);

    // Absolute URLs rewritten
    assert!(out.contains("/first-party/proxy?tsurl="));
    // Relative URLs left as-is
    assert!(out.contains("url(/local/border.png)"));
}
```

### Proxy Tests

From `crates/common/src/proxy.rs`:

```rust
#[test]
fn proxy_request_config_supports_streaming_and_headers() {
    let cfg = ProxyRequestConfig::new("https://example.com/asset")
        .with_body(vec![1, 2, 3])
        .with_header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )
        .with_streaming();

    assert_eq!(cfg.target_url, "https://example.com/asset");
    assert!(cfg.follow_redirects, "should follow redirects by default");
    assert!(cfg.forward_synthetic_id, "should forward synthetic id by default");
}

#[test]
fn header_copy_copies_curated_set() {
    let mut src = Request::new(Method::GET, "https://edge.example/first-party/proxy");
    src.set_header(HEADER_USER_AGENT, "UA/1.0");
    src.set_header(HEADER_ACCEPT, "image/*");
    src.set_header(HEADER_REFERER, "https://pub.example/page");

    let mut dst = Request::new(Method::GET, "https://cdn.example/a.png");
    copy_proxy_forward_headers(&src, &mut dst);

    assert_eq!(
        dst.get_header(HEADER_USER_AGENT).unwrap().to_str().unwrap(),
        "UA/1.0"
    );
}
```

## Test Helpers

The codebase provides test utilities in `crates/common/src/test_support.rs`:

```rust
use crate::test_support::tests::{create_test_settings, create_test_request};

// Create settings with test defaults
let settings = create_test_settings();

// Create a request with specific headers
let req = create_test_request(vec![
    (header::USER_AGENT, "Mozilla/5.0"),
    (header::COOKIE, "session=abc123"),
]);
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
# Run clippy with all checks
cargo clippy --all-targets --all-features --workspace --no-deps

# Fix clippy warnings automatically
cargo clippy --fix --allow-dirty
```

### Code Coverage

```bash
# Generate coverage report (requires cargo-tarpaulin)
cargo install cargo-tarpaulin
cargo tarpaulin --out Html
```

## CI/CD Testing

### GitHub Actions

Tests run automatically on pull requests and main branch commits. See `.github/workflows/` for the complete CI configuration.

```yaml
# Example workflow
name: Test
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-action@stable
      - name: Run tests
        run: cargo test
      - name: Run clippy
        run: cargo clippy --all-targets --all-features --workspace --no-deps
```

## Debugging Tests

### Enable Debug Logging

```bash
RUST_LOG=debug cargo test -- --nocapture
```

### Run Single Test with Full Output

```bash
cargo test test_name -- --nocapture --test-threads=1
```

### Viceroy Limitations

When tests fail, viceroy doesn't display line numbers. To debug:

1. Run with `--nocapture` to see all output
2. Add `log::info!()` statements to trace execution
3. Run specific tests in isolation with `--test-threads=1`

## Performance Testing

### Load Testing

```bash
# Using wrk
wrk -t12 -c400 -d30s http://localhost:7676/

# Using hey
hey -n 10000 -c 100 http://localhost:7676/

# Using k6
k6 run load-test.js
```

## Best Practices

1. **Test all new features** - Write tests alongside new code
2. **Use descriptive names** - `test_synthetic_id_generation_with_consent`
3. **Test edge cases** - Empty inputs, missing headers, invalid data
4. **Keep tests fast** - Mock external dependencies
5. **Use test helpers** - `create_test_settings()`, `create_test_request()`
6. **Assert specific values** - Not just `assert!(result.is_ok())`

## Next Steps

- Review [Architecture](/guide/architecture) for system design
- Configure your [Deployment](/guide/configuration)
- Learn about [Request Signing](/guide/request-signing)
