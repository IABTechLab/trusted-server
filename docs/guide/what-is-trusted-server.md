# What is Trusted Server?

Trusted Server is an open-source, cloud based orchestration framework and runtime for publishers. It moves code execution and operations that traditionally occurs in browsers (via 3rd party JS) to secure, zero-cold-start WASM binaries running in WASI supported environments.

Trusted Server is the new execution layer for the open-web, returning control of 1st party data, security, and overall user-experience back to publishers.

## Key Features

Trusted Server provides publishers benefits such as:

- Dramatically increased control over 3rd party data sharing (while maintaining user-privacy compliance such as GDPR through CMP integrations)
- Increased ad inventory revenue within cookie restricted (Safari Webkit) or other JS-challenged environments
- Ability to serve ALL assets under 1st party context with on-the-fly URL detection and rewriting via HTML/CSS stream inspection
- Cryptographically sign bid requests for downstream partners to verify requests originated from specific publisher's Trusted Server instance, preventing fraud and spoofing
- Creates deterministic, cryptographically Synthetic identifiers (Publisher Owned Synthetic IDs) without relying on third-party cookies
- Native integration with Prebid-Server and other third-party monetization and ID vendors
- Native support for parsing RSC (React Server Components) for Next.JS front-end origins
- Dynamic backend support for ease of integrating existing publisher environments
- Support for existing 3rd party JS workflows

### Privacy-Preserving

All tracking and data collection requires explicit user consent through GDPR compliance checks.

### Edge Computing

Currently runs on Fastly Compute platform, providing global low-latency performance. Initiatives for Cloudflare Worker support and Akamai's Fermyon SPIN support.

### Real-Time Bidding

Integrates with Prebid for seamless RTB workflows.

## Use Cases

- GDPR-compliant ad serving
- Privacy-safe user tracking
- Real-time bidding integration
- Edge-based ad delivery

## Next Steps

Continue to [Getting Started](/guide/getting-started) to begin using Trusted Server.
