# Testing

Learn how to test Trusted Server locally and in CI/CD.

## Testing absolute creative rewrite URLs

Creative rewrite tests should configure a distinct `publisher.public_origin` and assert the parsed URL origin, not only a `/first-party/...` path substring. Keep manual root-relative proxy and click fixtures for compatibility tests, because deployed creative output is absolute on the configured public origin.

## Test Infrastructure

### Viceroy

Viceroy is the local test runtime for Fastly Compute applications. It simulates the Fastly environment locally and is required for running the WASM crate tests.

```bash
# Install viceroy
cargo install viceroy --version 0.17.0 --locked --force

# Run Fastly/WASM crate tests (viceroy is invoked automatically via .cargo/config.toml runner)
cargo test-fastly
```

### Axum adapter tests

The Axum adapter runs as a native binary — no Viceroy or WASM toolchain needed:

```bash
cargo test-axum
```

### Test Organization

Tests are organized alongside source code in `#[cfg(test)]` modules:

```rust
// crates/trusted-server-core/src/ec/generation.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_ec_id() {
        let settings = create_test_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::COOKIE, "pub_userid=12345"),
        ]);

        let ec_id = generate_ec_id(&settings, &req)
            .expect("should generate EC ID");

        assert!(!ec_id.is_empty());
    }
}
```

## Running Tests

### Unit Tests

```bash
# Run Fastly/WASM crate tests (requires Viceroy)
cargo test-fastly

# Run Axum adapter tests (native)
cargo test-axum

# Run a specific test by name (Fastly/WASM)
cargo test-fastly test_generate_ec_id

# Run a specific test by name (Axum native)
cargo test-axum test_generate_ec_id

# Run tests for a specific crate (native)
cargo test -p trusted-server-core

# Run tests matching a pattern (Fastly/WASM)
cargo test-fastly ec
```

### Integration Tests

The integration test suite runs the full pipeline against Docker containers using both the Fastly (Viceroy) and Axum runtimes:

```bash
# Build both runtimes and run all integration tests
./scripts/integration-tests.sh

# Run a single test
./scripts/integration-tests.sh test_wordpress_axum
./scripts/integration-tests.sh test_wordpress_fastly
```

### Local Server Testing

**Axum dev server** (no Fastly CLI required):

```bash
# Start the Axum dev server
cargo run -p trusted-server-adapter-axum

# Test endpoints with curl
curl http://localhost:8787/.well-known/trusted-server.json
```

**Fastly Viceroy** (requires Fastly CLI):

```bash
# Start local Fastly simulator
fastly compute serve

# Test endpoints with curl
curl http://localhost:7676/.well-known/trusted-server.json
```

## Real Test Examples

### EC ID Tests

From `crates/trusted-server-core/src/ec/mod.rs`:

```rust
#[test]
fn test_get_ec_id_with_header() {
    let settings = create_test_settings();
    let req = create_test_request(vec![(
        HEADER_X_TS_EC,
        "existing_ec_id",
    )]);

    let ec_id = get_ec_id(&req)
        .expect("should get EC ID");
    assert_eq!(ec_id, Some("existing_ec_id".to_string()));
}

#[test]
fn test_get_ec_id_with_cookie() {
    let settings = create_test_settings();
    let req = create_test_request(vec![
        (header::COOKIE, "ts-ec=existing_cookie_id")
    ]);

    let ec_id = get_ec_id(&req)
        .expect("should get EC ID");
    assert_eq!(ec_id, Some("existing_cookie_id".to_string()));
}

#[test]
fn test_get_ec_id_none() {
    let req = create_test_request(vec![]);
    let ec_id = get_ec_id(&req)
        .expect("should handle missing ID");
    assert!(ec_id.is_none());
}
```

### Creative Rewriting Tests

From `crates/trusted-server-core/src/creative.rs`:

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

From `crates/trusted-server-core/src/proxy.rs`:

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
    assert!(cfg.forward_ec_id, "should forward EC ID by default");
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

The codebase provides test utilities in `crates/trusted-server-core/src/test_support.rs`:

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
# Run clippy for Fastly/WASM adapter
cargo clippy-fastly

# Run clippy for Axum native adapter
cargo clippy-axum

# Fix clippy warnings automatically (Axum)
cargo clippy-axum --fix --allow-dirty
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
# Example workflow (see .github/workflows/test.yml for the full version)
name: Test
on: [push, pull_request]
jobs:
  test-rust: # Fastly/WASM crates — requires Viceroy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-action@stable
      - name: Run tests
        run: cargo test-fastly
      - name: Run clippy
        run: cargo clippy-fastly

  test-axum: # Axum native adapter — no Viceroy needed
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build Axum adapter
        run: cargo build -p trusted-server-adapter-axum
      - name: Run Axum adapter tests
        run: cargo test-axum
```

## Debugging Tests

### Enable Debug Logging

```bash
RUST_LOG=debug cargo test-axum -- --nocapture
```

### Run Single Test with Full Output

```bash
cargo test-axum test_name -- --nocapture --test-threads=1
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
2. **Use descriptive names** - `test_ec_id_generation_with_consent`
3. **Test edge cases** - Empty inputs, missing headers, invalid data
4. **Keep tests fast** - Mock external dependencies
5. **Use test helpers** - `create_test_settings()`, `create_test_request()`
6. **Assert specific values** - Not just `assert!(result.is_ok())`

## Next Steps

- Review [Architecture](/guide/architecture) for system design
- Configure your [Deployment](/guide/configuration)
- Learn about [Request Signing](/guide/request-signing)
