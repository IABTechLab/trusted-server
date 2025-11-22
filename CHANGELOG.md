# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Implemented basic authentication for configurable endpoint paths (#73)
- Added integrations guide with example `testlight` integration

## [1.2.0] - 2025-10-14

### Changed

- Publisher origin backend now uses `publisher.origin_url` to dynamically create backends, deprecated `publisher.origin_backend` field
- Prebid backend now uses `prebid.server_url` to dynamically create backends, deprecated `prebid.prebid_backend` field
- Removed static backend definitions from `fastly.toml` for publisher and prebid

### Added

- Added `.rust-analyzer.json` for improved development environment support with Neovim/rust-analyzer

## [1.1.0] - 2025-10-05

### Added

- Added basic unit tests
- Added publisher config
- Add AI assist rules. Based on https://github.com/hashintel/hash
- Added ability to construct GAM requests from static permutive segments with test pages
- Add more complete e2e GAM (Google Ad Manager) integration with request construction and ad serving capabilities
- Add new partners.rs module for partner-specific configurations
- Created comprehensive publisher IDs audit document identifying hardcoded values
- Enabled first-party ad endpoints that rewrite creatives in first party domain
- Added first-party end point to proxy Prebid auctions
- Added Trusted Server TSJS SDK with bundled build, lint, and test tools for serving creatives in first-party domain

### Changed

- Upgrade to rust 1.90.0
- Upgrade to fastly-cli 12.0.0
- Changed to use constants for headers
- Changed to use log statements
- Updated fastly.toml for local development
- Changed to propagate server errors as HTTP errors
- Reworked Fastly routing so first-party endpoints and synthetic cookies stay in sync
- Added TypeScript CI lint, format, and test jobs for TSJS

### Fixed

- Rebuild when `TRUSTED_SERVER__*` env variables change

## [1.0.6] - 2025-05-29

### Changed

- Remove hard coded Fast ID in fastly.tom
- Updated README to better describe what Trusted Server does and high-level goal
- Use Rust toolchain version from .tool-versions for GitHub actions

## [1.0.5] - 2025-05-19

### Changed

- Refactor into crates to allow to separate Fastly implementation
- Remove references to POTSI
- Rename `potsi.toml` to `trusted-server.toml`

### Added

- Implemented GDPR consent for creating and passing synth headers

## [1.0.4] - 2025-04-29

### Added

- Implemented GDPR consent for creating and passing synth headers

## [1.0.3] - 2025-04-23

### Changed

- Upgraded to Fastly CLI v11.2.0

## [1.0.2] - 2025-03-28

### Added

- Documented project gogernance in [ProjectGovernance.md]
- Document FAQ for POC [FAQ_POC.md]

## [1.0.1] - 2025-03-27

### Changed

- Allow to templatize synthetic cookies

## [1.0.0] - 2025-03-26

### Added

- Initial implementation of Trusted Server

[Unreleased]: https://github.com/IABTechLab/trusted-server/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/IABTechLab/trusted-server/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/IABTechLab/trusted-server/compare/v1.0.6...v1.1.0
[1.0.6]: https://github.com/IABTechLab/trusted-server/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/IABTechLab/trusted-server/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/IABTechLab/trusted-server/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/IABTechLab/trusted-server/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/IABTechLab/trusted-server/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/IABTechLab/trusted-server/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/IABTechLab/trusted-server/releases/tag/v1.0.0
