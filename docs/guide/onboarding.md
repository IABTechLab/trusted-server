# New Engineer Onboarding Guide

This document is intended for internal onboarding. For public setup steps, start with
[Getting Started](/guide/getting-started). For test commands and workflows, use the
[Testing guide](/guide/testing).

Welcome to the Trusted Server project! This guide keeps internal onboarding notes concise and links out to the canonical docs.

## Table of Contents

1. [Start Here](#start-here)
2. [Local Setup Notes](#local-setup-notes)
3. [Codebase Pointers](#codebase-pointers)
4. [Development Workflow](#development-workflow)
5. [Debugging & Troubleshooting](#debugging--troubleshooting)
6. [Team & Governance](#team--governance)
7. [Resources & Getting Help](#resources--getting-help)
8. [Onboarding Checklist](#onboarding-checklist)

---

## Start Here

- Product overview: [What is Trusted Server](/guide/what-is-trusted-server)
- System design: [Architecture](/guide/architecture)
- Setup and deploy: [Getting Started](/guide/getting-started) and [Fastly Setup](/guide/fastly)
- Configure features: [Configuration](/guide/configuration) (see the Detailed Reference section)
- Testing workflow: [Testing guide](/guide/testing)
- Integrations: [Integrations Overview](/guide/integrations-overview) and [Integration Guide](/guide/integration-guide)

## Local Setup Notes

- Tool versions live in `.tool-versions` (use asdf or your preferred version manager).
- For docs site development, see `docs/README.md`.

## Codebase Pointers

| File                                         | Purpose                          |
| -------------------------------------------- | -------------------------------- |
| `crates/fastly/src/main.rs`                  | Request routing entry point      |
| `crates/common/src/publisher.rs`             | Publisher origin handling        |
| `crates/common/src/proxy.rs`                 | First-party proxy implementation |
| `crates/common/src/synthetic.rs`             | Synthetic ID generation          |
| `crates/common/src/integrations/registry.rs` | Integration module pattern       |
| `trusted-server.toml`                        | Application configuration        |

## Development Workflow

- Contribution process: [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
- Adding integrations: [Integration Guide](/guide/integration-guide)
- Request flows: [SEQUENCE.md](https://github.com/IABTechLab/trusted-server/blob/main/SEQUENCE.md)

## Debugging & Troubleshooting

- [Error Reference](/guide/error-reference)
- [Testing guide](/guide/testing)
- [Fastly Setup](/guide/fastly) (local simulator notes)

---

## Team & Governance

### Project Structure

The project follows IAB Tech Lab's open-source governance model:

- **Trusted Server Task Force**: Defines requirements and roadmap (meets biweekly)
- **Development Team**: Handles engineering implementation and releases

### Team Roles

| Role         | Responsibility                       |
| ------------ | ------------------------------------ |
| Project Lead | Overall project vision and direction |
| Developer    | Contributes code/docs                |

See [ProjectGovernance.md](https://github.com/IABTechLab/trusted-server/blob/main/ProjectGovernance.md) for full details.

### Key Contacts

| Role         | GitHub Handle                                                |
| ------------ | ------------------------------------------------------------ |
| Project Lead | [@jevansnyc](https://github.com/jevansnyc)                   |
| Developer    | [@aram356](https://github.com/aram356)                       |
| Developer    | [@ChristianPavilonis](https://github.com/ChristianPavilonis) |

### Meetings

<!-- TODO: Add actual meeting links and times -->

- **Task Force Meeting**: Biweekly (check calendar for schedule)
- **Development Team Standup**: Weekly (check calendar for schedule)

Ask your manager or onboarding buddy for calendar invites to relevant meetings.

---

## Resources & Getting Help

### Documentation

| Resource                                                                                  | Description                                   |
| ----------------------------------------------------------------------------------------- | --------------------------------------------- |
| [README.md](https://github.com/IABTechLab/trusted-server/blob/main/README.md)             | Project overview and setup                    |
| [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md) | Contribution guidelines                       |
| [AGENTS.md](https://github.com/IABTechLab/trusted-server/blob/main/AGENTS.md)             | AI assistant guidance / architecture overview |
| [SEQUENCE.md](https://github.com/IABTechLab/trusted-server/blob/main/SEQUENCE.md)         | Request flow diagrams                         |
| [FAQ_POC.md](https://github.com/IABTechLab/trusted-server/blob/main/FAQ_POC.md)           | Frequently asked questions                    |

For docs site development, see `docs/README.md`.

### Getting Help

- **GitHub Issues**: For bugs, feature requests, and questions
- **Task Force Meetings**: Biweekly meetings for roadmap discussions
- **Code Review**: Submit PRs for feedback from maintainers

### External Resources

- [Fastly Compute Documentation](https://developer.fastly.com/learning/compute/)
- [Rust Book](https://doc.rust-lang.org/book/)
- [WebAssembly Overview](https://webassembly.org/)
- [OpenRTB Specification](https://iabtechlab.com/standards/openrtb/)

---

## Onboarding Checklist

Use this checklist to track your onboarding progress:

### Access & Accounts

- [ ] Get GitHub access to [IABTechLab/trusted-server](https://github.com/IABTechLab/trusted-server)
- [ ] Get access to the [Trusted Server project board](https://github.com/orgs/IABTechLab/projects/3)
- [ ] Create a [Fastly account](https://manage.fastly.com) and obtain an API token
- [ ] Join the Slack workspace and `#trusted-server-internal` channel
- [ ] Get calendar invites for Task Force and Development Team meetings

### Environment Setup

- [ ] Complete the setup steps in [Getting Started](/guide/getting-started)
- [ ] Run the test flow in the [Testing guide](/guide/testing)
- [ ] Start the local server (see [Getting Started](/guide/getting-started))

### Codebase Exploration

- [ ] Read through `main.rs` to understand request routing
- [ ] Trace a request through `publisher.rs` and `proxy.rs`
- [ ] Understand synthetic ID generation in `synthetic.rs`
- [ ] Review an existing integration (e.g., `prebid.rs`)

### Documentation & Contribution

- [ ] Read `CONTRIBUTING.md` for PR guidelines
- [ ] Browse the [documentation site guides](/guide/getting-started)
- [ ] Make a small contribution (fix a typo, add a test, etc.)

---

Welcome aboard! Don't hesitate to ask questions - we're here to help you succeed.
