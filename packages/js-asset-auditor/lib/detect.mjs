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

const PREBID_SUFFIXES = [
  "/prebid.js",
  "/prebid.min.js",
  "/prebid-loader.js",
  "/prebid-load.js",
  "/prebid-wrapper.js",
  "/prebidjs.js",
  "/prebidjs.min.js",
];

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
      url.pathname.endsWith("/gtm.js"),
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
      if (url.hostname.endsWith(".edge.permutive.app")) {
        result.organization_id = url.hostname.replace(".edge.permutive.app", "");
      }
      const filename = url.pathname.split("/").pop() || "";
      const workspaceMatch = filename.match(/^(.+)-web\.js$/);
      if (workspaceMatch) {
        result.workspace_id = workspaceMatch[1];
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
    match: (url) => {
      const lowerPathname = url.pathname.toLowerCase();
      return PREBID_SUFFIXES.some((suffix) => lowerPathname.endsWith(suffix));
    },
    extract: () => ({}),
    applyRuntimeSignals: (entry, runtimeSignals) => {
      const bidders = runtimeSignals?.prebidBidders ?? [];
      if (bidders.length > 0 && !("bidders" in entry.extracted)) {
        entry.extracted.bidders = bidders;
      }
    },
    defaults: {
      timeout_ms: 1000,
      debug: false,
    },
    todos: (extracted) => {
      const missing = ["server_url"];
      if (!Array.isArray(extracted.bidders) || extracted.bidders.length === 0) {
        missing.push("bidders");
      }
      return missing;
    },
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

export function detectIntegrations(rawUrls, runtimeSignals = {}) {
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
        const existing = detected.get(pattern.id);
        existing.sourceUrls.push(rawUrl);
        const newExtracted = pattern.extract(url);
        for (const [key, value] of Object.entries(newExtracted)) {
          if (!(key in existing.extracted)) existing.extracted[key] = value;
        }
      } else {
        const extracted = pattern.extract(url);
        const todos = [];

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

      break;
    }
  }

  for (const [id, entry] of detected) {
    const pattern = INTEGRATION_PATTERNS.find((candidate) => candidate.id === id);
    if (pattern?.applyRuntimeSignals) {
      pattern.applyRuntimeSignals(entry, runtimeSignals);
    }
    entry.todos =
      typeof pattern?.todos === "function"
        ? pattern.todos(entry.extracted)
        : [...(pattern?.todos ?? [])];
  }

  return {
    integrations: [...detected.values()],
  };
}

// ---------------------------------------------------------------------------
// Config generation
// ---------------------------------------------------------------------------

function formatTomlString(value) {
  return JSON.stringify(value);
}

function formatTomlValue(value) {
  if (typeof value === "string") return formatTomlString(value);
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return String(value);
  if (Array.isArray(value)) {
    return `[${value.map((entry) => formatTomlString(entry)).join(", ")}]`;
  }
  return String(value);
}

function isIntegrationConfigComplete(integration) {
  return integration.category === "full" || integration.todos.length === 0;
}

export function generateConfig(domain, targetUrl, detectionResult) {
  const today = new Date().toISOString().slice(0, 10);
  let toml = "";

  toml += `# Generated by js-asset-auditor on ${today}\n`;
  toml += `# Source URL: ${targetUrl}\n`;
  toml += `#\n`;
  toml += `# Review all values before deploying. Fields marked TODO need manual input.\n`;
  toml += `# Commented-out fields show defaults — uncomment to override.\n`;
  toml += `\n`;

  toml += `[publisher]\n`;
  toml += `domain = ${formatTomlString(domain)}\n`;
  toml += `# cookie_domain = ${formatTomlString(`.${domain}`)}\n`;
  toml += `# origin_url = ${formatTomlString(`https://origin.${domain}`)}\n`;
  toml += `# proxy_secret = ${formatTomlString("change-me")}\n`;

  for (const integration of detectionResult.integrations) {
    toml += `\n`;
    toml += `[integrations.${integration.id}]\n`;
    toml += `enabled = ${isIntegrationConfigComplete(integration)}\n`;

    for (const [key, value] of Object.entries(integration.extracted)) {
      toml += `${key} = ${formatTomlValue(value)}  # auto-detected\n`;
    }

    for (const field of integration.todos) {
      toml += `# ${field} = ${formatTomlString("")}  # TODO: set your ${integration.label} ${field}\n`;
    }

    for (const [key, value] of Object.entries(integration.defaults)) {
      if (key in integration.extracted) continue;
      toml += `# ${key} = ${formatTomlValue(value)}\n`;
    }
  }

  return toml;
}
