# Roadmap

The Trusted Server roadmap is organized around three strategic initiatives aligned with our GitHub project structure. This page outlines features currently in development and planned for 2026.

## Overview

Our development is focused on three key areas:

1. **Edge Platform** - Code execution, proxy capabilities, and runtime support
2. **Monetization** - Ad serving, identity, and bidstream integration
3. **Transaction Trust** - Attestation, measurement, and fraud prevention

---

## 🚀 Edge Platform: Code Execution, Proxy, and Runtime

### In-Flight

**Cloudflare Workers Support**

- Adapt `/common/` crate for Cloudflare Workers runtime
- Create platform-specific adapter in `/cloudflare/` crate
- Maintain feature parity with Fastly Compute
- Enable multi-platform publisher deployments

**Next.js RSC (React Server Components) Enhancements**

- Improved streaming support for App Router
- Better hydration data parsing
- Enhanced origin URL rewriting

### Planned for 2026

**SPIN + Akamai EdgeWorkers Support**

- Port Trusted Server to Fermyon SPIN runtime
- Enable deployment on Akamai's edge platform
- WebAssembly Component Model adoption
- Cross-platform WASI compatibility

**Encoding/Decoding Streaming Improvements**

- Enhanced Gzip, Brotli, Deflate streaming support
- Memory-efficient large file processing
- Improved compression passthrough modes

**WebSockets Support**

- Real-time bidirectional communication at the edge
- Server-sent events (SSE) for live updates
- Enhanced streaming auction support

**Header Configuration Enhancements**

- Advanced CORS policy management
- Content Security Policy (CSP) templating
- X-Frame-Options and crossorigin attribute control
- Security header automation

---

## 💰 Monetization: SSPs, DSPs, PBS, Ad-Servers, ID Providers

### In-Flight

**Prebid Server Integration Enhancements**

- Improved OpenRTB 2.x support
- Enhanced bidder configuration
- Better timeout management

**Permutive Audience Data**

- First-party audience segmentation
- Real-time cohort updates
- Secure Signals integration

### Planned for 2026

**Full Google Ad Manager (GAM) Support**

- Complete GAM publisher integration
- Dynamic ad slot management
- Programmatic guaranteed support
- First-party GAM reporting

**Amazon Publisher Services (APS) Integration**

- Transparent Ad Marketplace (TAM) support
- A9 bidding integration
- First-party APS header bidding

**Kargo SSP Integration**

- Native Kargo bid adapter
- First-party creative rendering
- Enhanced viewability tracking

**Additional SSP Integrations**

- Index Exchange
- OpenX
- PubMatic
- Other major SSPs

**Prebid Server Rust Port**

- Native Rust implementation of Prebid Server
- Embedded WASM-based bidder adapters
- Zero-cold-start header bidding
- Reduce external dependencies

**Better Synthetic ID Syncing**

- Enhanced ID sync with downstream partners
- Batch sync optimization
- Partner ID mapping tables
- Sync status monitoring

**Standalone Synthetic ID Service**

- Dedicated ID generation microservice
- Cross-publisher ID graph support
- Privacy-preserving ID resolution
- API-first architecture

**OpenRTB Geo Signal Enrichment**

- Push X-Geo signals into bidstream
- City, continent, coordinates, metro-code (DMA)
- Country and region data
- IP geolocation integration

**Agentic Frameworks for Dynamic Ad Optimization**

- LLM-powered ad relevance scoring
- Context-aware creative selection
- Real-time optimization agents

**Content Protection & RSL Protocol**

- Really Simple Licensing (RSL) integration
- AI crawler detection and blocking
- Content usage attribution
- Licensing metadata injection

---

## 🛡️ Transaction Trust: Attestation, Measurement, IVT & Fraud Prevention

### In-Flight

**Request Signing & JWKS**

- Ed25519 cryptographic signing
- JSON Web Key Set (JWKS) discovery
- Key rotation automation

### Planned for 2026

**Advanced Observability and Security Tooling**

A comprehensive creative forensics engine for ad behavior monitoring and fraud prevention:

**Capabilities:**

- Real-time creative behavior analysis using headless browser automation
- Network traffic interception and validation
- DOM manipulation detection
- Unauthorized data exfiltration prevention
- Malvertising and malicious script identification

**Business Impact:**

- **Compliance Protection**: Prevent GDPR violations from third-party creatives (potential $2.3M+ fine avoidance)
- **Revenue Protection**: Detect bid manipulation and auction fraud ($400K+ annual recovery potential)
- **User Trust**: Block malvertising before it reaches users (prevent user exodus, retain 100K+ users)
- **Brand Safety**: Real-time creative quality assurance

**Technical Implementation:**

- Headless browser integration (Playwright/Puppeteer)
- Chrome DevTools Protocol (CDP) for network inspection
- Edge + backend hybrid architecture
- Real-time alerting and blocking

**LLM-Crawler Detection & .md Versions**

- Detect AI training crawlers (GPTBot, Claude-Web, etc.)
- Serve markdown versions for authorized AI consumption
- Track AI content usage with RSL metadata
- Content attribution and licensing

**JavaScript Parsing/Evaluation Engine**

- Safe JS execution sandbox at the edge
- Creative script analysis before rendering
- Malicious code detection
- Dynamic creative validation

**Verified Builds & Attestation**

- Reproducible WASM builds
- Cryptographic build verification
- Transparency logs for deployed code
- Publisher trust attestation

**Invalid Traffic (IVT) Detection**

- Bot traffic identification
- Sophisticated invalid traffic (SIVT) detection
- Fraud score attribution
- Real-time traffic filtering

**Consent Management Platform (CMP) Enhancements**

- Deeper Didomi integration
- OneTrust support
- Quantcast CMP integration
- TCF 2.2 full compliance

---

## Timeline & Priorities

### Q1 2026

- Cloudflare Workers Support (production ready)
- Full GAM Integration
- Advanced Observability tooling (beta)

### Q2 2026

- SPIN + Akamai Support
- Prebid Server Rust Port (alpha)
- Standalone Synthetic ID Service

### Q3 2026

- Additional SSP Integrations (Index, OpenX, PubMatic)
- LLM-Crawler Detection
- Verified Builds

### Q4 2026

- Agentic frameworks for ad optimization
- IVT detection (production)
- WebSockets support

---

## Contributing

Interested in contributing to these initiatives? Check out our [GitHub Issues](https://github.com/IABTechLab/trusted-server/issues) for specific tasks and discussions.

### How to Contribute

1. **Pick an Initiative**: Choose from the roadmap above
2. **Check Issues**: Look for existing issues or create a new one
3. **Discuss Design**: Propose your approach in the issue
4. **Submit PR**: Follow our [contribution guidelines](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)

---

## Feedback

Have ideas for the roadmap? [Open an issue](https://github.com/IABTechLab/trusted-server/issues/new) or join the discussion in our community channels.

## Next Steps

- Review [Architecture](/guide/architecture) to understand the system
- Explore [Integration Guide](/guide/integration-guide) to build custom integrations
- Check [Configuration](/guide/configuration) for deployment options
