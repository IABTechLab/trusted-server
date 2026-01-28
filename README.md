# Trusted Server

Trusted Server is an open-source, cloud-based orchestration framework and runtime for publishers. It moves code execution and operations that traditionally occur in browsers (via 3rd party JS) to secure, zero-cold-start [WASM](https://webassembly.org) binaries running in [WASI](https://github.com/WebAssembly/WASI) supported environments.

**Key benefits:**

- Dramatically increases control over data sharing while maintaining privacy compliance
- Increases revenue from inventory in cookie-restricted or non-JS environments
- Serves all assets under first-party context
- Provides secure cryptographic functions for trust across the programmatic ad ecosystem

At this time, Trusted Server is designed to work with [Fastly Compute](https://www.fastly.com/products/compute).

## Documentation

📚 **[View Full Documentation](docs/guide/getting-started.md)**

| Guide                                                | Description                                  |
| ---------------------------------------------------- | -------------------------------------------- |
| [Getting Started](docs/guide/getting-started.md)     | Installation, setup, and first deployment    |
| [Fastly Setup](docs/guide/fastly.md)                 | Fastly account, Compute service, and origins |
| [Configuration](docs/guide/configuration.md)         | Configuration options and settings           |
| [Synthetic IDs](docs/guide/synthetic-ids.md)         | Privacy-preserving identifier generation     |
| [Ad Serving](docs/guide/ad-serving.md)               | Ad server integration and setup              |
| [First-Party Proxy](docs/guide/first-party-proxy.md) | Proxy configuration for first-party context  |
| [Request Signing](docs/guide/request-signing.md)     | Cryptographic request signing with Ed25519   |
| [API Reference](docs/guide/api-reference.md)         | Complete API endpoint documentation          |
| [Integration Guide](docs/guide/integration-guide.md) | Building custom integrations                 |

## License

Apache License 2.0
