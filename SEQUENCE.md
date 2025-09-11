# Trusted Server — First-Party Proxying Flow

This sequence diagram shows how the Trusted Server proxies a publisher page, injects TSJS, rewrites publisher HTML, runs the auction path(s), and rewrites ad creative resources to the unified first‑party proxy.

Tip: In VS Code, open this file and use the Markdown preview. If Mermaid isn’t rendering, install a Mermaid preview extension (e.g., “Markdown Preview Mermaid Support”) or use the built‑in preview if available.

```mermaid
%%{init: {
  "theme": "base",
  "themeVariables": {
    "actorBkg": "#eef5ff",
    "actorBorderColor": "#3b82f6",
    "actorTextColor": "#0b3d91",
    "signalColor": "#334155",
    "signalTextColor": "#334155",
    "sequenceNumberColor": "#64748b"
  }
}}%%
sequenceDiagram
  autonumber

  box rgb(235,245,255) Browser
    actor U as User Browser
  end
  participant JS as TSJS
  box rgb(255,240,235) Edge
    participant TS as Trusted Server
  end
  box rgb(240,255,240) Origin
    participant OR as Publisher Origin
  end
  participant PBS as Prebid Server
  participant DSP as DSP
  participant CS as Creative Server

  %% Publisher page load
  rect rgb(250,250,205)
    U->>TS: GET https://publisher/path
    TS->>OR: GET http://origin/path
    OR-->>TS: 200 HTML
    TS->>TS: Inject TSJS and rewrite prebid if enabled and replace origin URLs
    TS-->>U: 200 HTML rewritten
  end

  %% TSJS bootstrap
  rect rgb(220,255,235)
    U->>TS: GET /static/tsjs-core.min.js
    TS-->>U: 200 JS
    JS->>JS: Discover ad units and render placeholders
  end

  %% Ad request
  rect rgb(235,245,255)
    JS->>TS: GET /first-party/ad
    TS->>PBS: POST /openrtb2/auction
    PBS->>DSP: POST bid request
    DSP-->>PBS: 200 bid response
    PBS-->>TS: 200 JSON bids
    TS->>TS: Extract creative HTML from Prebid Server
    TS->>TS: Rewrite resource in first party domain
    TS-->>JS: 200 HTML creative
    JS->>U: Inject creative in iframe
  end

  %% Creative subresources via first-party proxy
  rect rgb(255,245,230)
    U->>TS: GET first party proxy with token
    TS->>CS: GET target url
    CS-->>TS: 200 image or html
    opt HTML response
      TS->>TS: Rewrite creative HTML again in first party domain
      TS-->>U: 200 text/html
    end
    opt Image response
      TS->>TS: Ensure content type and log small pixel heuristics
      TS-->>U: 200 image/*
    end
  end
```

## Notes
- TSJS
  - Served first-party at `/static/tsjs-core.min.js` (and `/static/tsjs-ext.min.js` if prebid auto-config is enabled).
  - Discovers ad units and renders placeholders; either uses slot-level HTML (`/first-party/ad`) or JSON auction (`/third-party/ad`).
- Publisher HTML Rewriting
  - Injects TSJS loader and rewrites absolute URLs from origin domain to first-party domain during streaming.
- Creative HTML Rewriting
  - Rewrites `<img>`, `srcset`, and `<iframe>` URLs to `/first-party/proxy?u=<token>`.
  - `<token>` is an encrypted+authenticated value using XChaCha20-Poly1305 with `publisher.proxy_secret`.
- Unified Proxy
  - `/first-party/proxy?u=<token>` decrypts to the target URL, proxies it, rewrites HTML responses again, and ensures image content-type if missing (also logs likely 1×1 pixels by heuristics).
- Prebid Server
  - OpenRTB requests are posted to `prebid.server_url`; responses are transformed to ensure first-party serving (HTML `adm` or JSON fields like `nurl/burl`).
