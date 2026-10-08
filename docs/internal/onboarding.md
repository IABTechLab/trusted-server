# Maintainer Onboarding

This document collects public repository and setup pointers for maintainers.
For project usage, start with the public guides.

## Start here

- [What is Trusted Server?](../guide/what-is-trusted-server.md)
- [Architecture](../guide/architecture.md)
- [Getting Started](../guide/getting-started.md)
- [Configuration](../guide/configuration.md)
- [Testing](../guide/testing.md)
- [Integrations Overview](../guide/integrations-overview.md)
- [Integration Guide](../guide/integration-guide.md)

## Local setup

- Tool versions are defined in `.tool-versions`.
- Documentation development instructions are in `docs/README.md`.
- Contribution requirements are in
  [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md).

## Codebase pointers

| Path                                                      | Purpose                           |
| --------------------------------------------------------- | --------------------------------- |
| `crates/trusted-server-adapter-fastly/src/main.rs`        | Request routing entry point       |
| `crates/trusted-server-core/src/publisher.rs`             | Publisher origin handling         |
| `crates/trusted-server-core/src/proxy.rs`                 | First-party proxy implementation  |
| `crates/trusted-server-core/src/ec/`                      | EC identity subsystem             |
| `crates/trusted-server-core/src/integrations/registry.rs` | Integration module registry       |
| `trusted-server.example.toml`                             | Example application configuration |

## Development workflow

- Follow [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
  when preparing changes.
- Use the [Testing guide](../guide/testing.md) for test commands.
- Use the [Integration Guide](../guide/integration-guide.md) when adding an
  integration.
- Report bugs and request changes through the repository's public
  [GitHub issues](https://github.com/IABTechLab/trusted-server/issues).
