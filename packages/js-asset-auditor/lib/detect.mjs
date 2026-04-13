// JS Asset Auditor — Integration Detection & Config Generation
//
// Detects known integrations from raw script URLs captured during a page sweep,
// then generates a trusted-server.toml with appropriate [integrations.*] sections.
//
// Integration patterns are derived from the Rust source in
// crates/trusted-server-core/src/integrations/.

// ---------------------------------------------------------------------------
// Integration pattern registry
// ---------------------------------------------------------------------------

const PREBID_SUFFIXES = ["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"];

const INTEGRATION_PATTERNS = [
  {
    id: "gpt",
    label: "Google Publisher Tags",
    match: (url) =>
      url.hostname === "securepubads.g.doubleclick.net" &&
      url.pathname.startsWith("/tag/js/gpt"),
    extract: (url) => ({
      script_url: `${url.origin}${url.pathname}`,
    }),
    defaults: {
      cache_ttl_seconds: 3600,
      rewrite_script: true,
    },
    todos: [],
    category: "full",
  },
  {
    id: "google_tag_manager",
    label: "Google Tag Manager",
    match: (url) =>
      url.hostname === "www.googletagmanager.com" &&
      url.pathname.includes("/gtm.js"),
    extract: (url) => {
      const containerId = url.searchParams.get("id");
      return containerId ? { container_id: containerId } : {};
    },
    defaults: {},
    todos: (extracted) => (extracted.container_id ? [] : ["container_id"]),
    category: "partial",
  },
  {
    id: "didomi",
    label: "Didomi Consent",
    match: (url) =>
      url.hostname === "sdk.privacy-center.org" ||
      url.hostname === "api.privacy-center.org",
    extract: () => ({}),
    defaults: {
      sdk_origin: "https://sdk.privacy-center.org",
      api_origin: "https://api.privacy-center.org",
    },
    todos: [],
    category: "full",
  },
  {
    id: "datadome",
    label: "DataDome Bot Protection",
    match: (url) =>
      url.hostname === "js.datadome.co" ||
      url.hostname === "api-js.datadome.co",
    extract: () => ({}),
    defaults: {
      sdk_origin: "https://js.datadome.co",
      api_origin: "https://api-js.datadome.co",
      cache_ttl_seconds: 3600,
      rewrite_sdk: true,
    },
    todos: [],
    category: "full",
  },
  {
    id: "lockr",
    label: "Lockr Identity",
    match: (url) => {
      const href = url.href.toLowerCase();
      return (
        (url.hostname.includes("aim.loc.kr") ||
          url.hostname.includes("identity.loc.kr")) &&
        href.includes("identity-lockr") &&
        href.endsWith(".js")
      );
    },
    extract: (url) => ({
      sdk_url: url.href,
    }),
    defaults: {
      api_endpoint: "https://identity.loc.kr",
      cache_ttl_seconds: 3600,
      rewrite_sdk: true,
    },
    todos: ["app_id"],
    category: "partial",
  },
  {
    id: "permutive",
    label: "Permutive DMP",
    match: (url) =>
      (url.hostname.endsWith(".edge.permutive.app") ||
        url.hostname === "cdn.permutive.com") &&
      url.pathname.endsWith("-web.js"),
    extract: (url) => {
      const result = {};
      // Extract organization_id from subdomain: {org}.edge.permutive.app
      if (url.hostname.endsWith(".edge.permutive.app")) {
        result.organization_id = url.hostname.replace(".edge.permutive.app", "");
      }
      // Extract workspace_id from filename: /{workspace}-web.js
      const filename = url.pathname.split("/").pop() || "";
      const wsMatch = filename.match(/^(.+)-web\.js$/);
      if (wsMatch) {
        result.workspace_id = wsMatch[1];
      }
      return result;
    },
    defaults: {
      api_endpoint: "https://api.permutive.com",
      secure_signals_endpoint: "https://secure-signals.permutive.app",
    },
    todos: (extracted) => {
      const missing = [];
      if (!extracted.organization_id) missing.push("organization_id");
      if (!extracted.workspace_id) missing.push("workspace_id");
      return missing;
    },
    category: "partial",
  },
  {
    id: "prebid",
    label: "Prebid Header Bidding",
    match: (url) => PREBID_SUFFIXES.some((s) => url.pathname.endsWith(s)),
    extract: () => ({}),
    defaults: {
      timeout_ms: 1000,
      debug: false,
    },
    todos: ["server_url", "bidders"],
    category: "detect_only",
  },
  {
    id: "aps",
    label: "Amazon Publisher Services",
    match: (url) =>
      url.hostname === "c.amazon-adsystem.com" &&
      url.pathname.includes("/apstag"),
    extract: () => ({}),
    defaults: {
      endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid",
      timeout_ms: 1000,
    },
    todos: ["pub_id"],
    category: "detect_only",
  },
];

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

export function detectIntegrations(rawUrls) {
  const detected = new Map();

  for (const rawUrl of rawUrls) {
    let url;
    try {
      url = new URL(rawUrl);
    } catch {
      continue;
    }

    for (const pattern of INTEGRATION_PATTERNS) {
      if (!pattern.match(url)) continue;

      if (detected.has(pattern.id)) {
        // Merge: accumulate source URLs, fill in missing extracted fields
        const existing = detected.get(pattern.id);
        existing.sourceUrls.push(rawUrl);
        const newExtracted = pattern.extract(url);
        for (const [key, value] of Object.entries(newExtracted)) {
          if (!(key in existing.extracted)) existing.extracted[key] = value;
        }
      } else {
        const extracted = pattern.extract(url);
        const todos =
          typeof pattern.todos === "function"
            ? pattern.todos(extracted)
            : [...pattern.todos];

        detected.set(pattern.id, {
          id: pattern.id,
          label: pattern.label,
          category: pattern.category,
          extracted,
          defaults: { ...pattern.defaults },
          todos,
          sourceUrls: [rawUrl],
        });
      }

      break; // Each URL matches at most one integration
    }
  }

  // Recalculate dynamic todos after merging
  for (const [id, entry] of detected) {
    const pattern = INTEGRATION_PATTERNS.find((p) => p.id === id);
    if (pattern && typeof pattern.todos === "function") {
      entry.todos = pattern.todos(entry.extracted);
    }
  }

  return {
    integrations: [...detected.values()],
  };
}

// ---------------------------------------------------------------------------
// Config generation
// ---------------------------------------------------------------------------

function formatTomlValue(value) {
  if (typeof value === "string") return `"${value}"`;
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return String(value);
  if (Array.isArray(value))
    return `[${value.map((v) => `"${v}"`).join(", ")}]`;
  return String(value);
}

export function generateConfig(domain, targetUrl, detectionResult) {
  const today = new Date().toISOString().slice(0, 10);
  let toml = "";

  // Header
  toml += `# Generated by js-asset-auditor on ${today}\n`;
  toml += `# Source URL: ${targetUrl}\n`;
  toml += `#\n`;
  toml += `# Review all values before deploying. Fields marked TODO need manual input.\n`;
  toml += `# Commented-out fields show defaults — uncomment to override.\n`;
  toml += `\n`;

  // Publisher section
  toml += `[publisher]\n`;
  toml += `domain = "${domain}"\n`;
  toml += `# cookie_domain = ".${domain}"\n`;
  toml += `# origin_url = "https://origin.${domain}"\n`;
  toml += `# proxy_secret = "change-me"\n`;

  // Integration sections
  for (const integration of detectionResult.integrations) {
    toml += `\n`;
    toml += `[integrations.${integration.id}]\n`;
    toml += `enabled = true\n`;

    // Auto-extracted fields
    for (const [key, value] of Object.entries(integration.extracted)) {
      toml += `${key} = ${formatTomlValue(value)}  # auto-detected\n`;
    }

    // TODO fields
    for (const field of integration.todos) {
      toml += `${field} = ""  # TODO: set your ${integration.label} ${field}\n`;
    }

    // Default fields (commented out)
    for (const [key, value] of Object.entries(integration.defaults)) {
      // Skip if already in extracted
      if (key in integration.extracted) continue;
      toml += `# ${key} = ${formatTomlValue(value)}\n`;
    }
  }

  return toml;
}
