
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- for new features

### Changed
-  for changes in existing functionality

### Deprecated 
- for soon-to-be removed features.

### Removed 
- for now removed features

### Fixed
- for any bug fixes

### Security
- in case of vulnerabilities

## [Unreleased]

### Added
- Added basic unit tests
- Added publisher config
- Add AI assist rules. Based on https://github.com/hashintel/hash
- Added ability to construct GAM requests from static permutive segments with test pages

### Changed
- Upgrade to rust 1.87.0
- Upgrade to fastly-cli 11.3.0
- Changed to use constants for headers
- Changed to use log statements
- Updated fastly.toml for local development
- Changed to propagate server errors as HTTP errors

### Fixed
- Rebuild when `TRUSTED_SERVER__*` env variables change 

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

[Unreleased]:https://github.com/IABTechLab/trusted-server/compare/v1.0.6...HEAD
[1.0.6]:https://github.com/IABTechLab/trusted-server/compare/v1.0.5...v1.0.6
[1.0.5]:https://github.com/IABTechLab/trusted-server/compare/v1.0.4...v1.0.5
[1.0.4]:https://github.com/IABTechLab/trusted-server/compare/v1.0.3...v1.0.4
[1.0.3]:https://github.com/IABTechLab/trusted-server/compare/v1.0.2...v1.0.3
[1.0.2]:https://github.com/IABTechLab/trusted-server/compare/v1.0.1...v1.0.2
[1.0.1]:https://github.com/IABTechLab/trusted-server/compare/v1.0.0...v1.0.1
[1.0.0]:https://github.com/IABTechLab/trusted-server/releases/tag/v1.0.0
