---
layout: home

hero:
  name: "Trusted Server"
  text: "The New Execution Layer for Publishers"
  tagline: "A publisher-owned control plane for monetization, performance, and governance.


  One integration point across your stack. Faster pages by reducing third-party browser execution. Policy, consent, and auditability partners can trust."
  image:
    src: /images/hero-graphic.jpeg
    alt: Trusted Server Control Plane
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/IABTechLab/trusted-server 
features:
  - title: Edge Cookie (EC) Generation
    details: HMAC-based edge cookies minted by the publisher; downstream use determined by deployer configuration
  - title: Consent Signal Handling
    details: Extraction, decoding (TCF v2 format, GPP, GPC), and enforcement logic applied to ad serving decisions
  - title: Edge Computing
    details: Runs on Fastly Compute for low-latency, high-performance ad serving at the edge
  - title: Real-Time Bidding
    details: Prebid integration for real-time bidding workflows
  - title: Flexible Configuration
    details: External configuration via TOML files for customization and deployment
  - title: Configurable Consent Handling
    details: Trusted Server forwards available consent signals (TCF v2 format, GPP, GPC), and the deployer configures jurisdiction lists, signal interpretation, and conflict resolution
---
