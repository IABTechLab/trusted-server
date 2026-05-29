# PR #680 Reviewer Findings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the two reviewer-required findings from PR #680 plus low-effort cleanups: consolidate slot config into `trusted-server.toml`, namespace `window.__ts*` globals under `window._ts`, and fix the TypeScript `formats` type cast and `ts_initial` hardcoded string.

**Architecture:** Slot templates move from the standalone `creative-opportunities.toml` (embedded via `include_str!`) into the `[creative_opportunities]` section of `trusted-server.toml`, using the existing `vec_from_seq_or_map` deserializer pattern already used for `BID_PARAM_ZONE_OVERRIDES`. The window globals rename is a coordinated change across `gpt_bootstrap.js`, `index.ts`, and `publisher.rs` — all three must change together since they share a runtime contract.

**Tech Stack:** Rust (serde, toml), TypeScript, vanilla JS, `cargo test --workspace`, `npx vitest run`

---

## Context for all tasks

- **Branch:** create `fix/pr680-review-findings` off `server-side-ad-templates-impl` before starting
- **Current codebase:** `crates/trusted-server-core/`, `crates/trusted-server-adapter-fastly/`, `crates/js/lib/`
- **CI gates:** `cargo fmt`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, `npx vitest run`, `npm run format`
- **Error handling:** use `error-stack` (`Report<E>`), not anyhow. Use `derive_more::Display`, not thiserror.
- **No `unwrap()` in production code** — use `expect("should ...")`.
- **Do not** add `println!` / `eprintln!` — use `log::` macros.

---

## Task 1: Consolidate slot config into `trusted-server.toml`

**What:** Delete `creative-opportunities.toml`. Move `[[slot]]` arrays into `trusted-server.toml` as `[[creative_opportunities.slot]]`. Wire the `vec_from_seq_or_map` deserializer so env var JSON blobs also work. Remove the `SLOTS_FILE` static and `include_str!` from `main.rs`. Update `build.rs` to validate slot IDs from settings instead of a separate file.

**Files:**
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-core/build.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs` (function signatures)
- Modify: `trusted-server.toml`
- Delete: `creative-opportunities.toml`

**Steps:**

- [ ] **Step 1: Create the branch**

```bash
git checkout -b fix/pr680-review-findings
```

- [ ] **Step 2: Add `Serialize` and `slot` field to structs**

In `crates/trusted-server-core/src/creative_opportunities.rs`:

1. Add `Serialize` to `CreativeOpportunitySlot` derive — it already has `#[serde(skip, default)]` on `compiled_patterns` so that field won't serialize.

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeOpportunitySlot { ... }
```

Also add `Serialize` to `CreativeOpportunityFormat`, `SlotProviders`, `ApsSlotParams` (any struct used inside `CreativeOpportunitySlot`).

2. Add a `slot` field to `CreativeOpportunitiesConfig`:

```rust
use crate::settings::vec_from_seq_or_map;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreativeOpportunitiesConfig {
    pub gam_network_id: String,
    #[serde(default)]
    pub auction_timeout_ms: Option<u32>,
    #[serde(default = "PriceGranularity::dense")]
    pub price_granularity: PriceGranularity,
    /// Slot templates. Empty = feature disabled.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub slot: Vec<CreativeOpportunitySlot>,
}
```

Note: the field is named `slot` (not `slots`) to match the TOML key `[[creative_opportunities.slot]]`.

- [ ] **Step 3: Delete `CreativeOpportunitiesFile`**

Remove the `CreativeOpportunitiesFile` struct and its `impl` from `creative_opportunities.rs`. The `compile` logic moves to a free function or into `CreativeOpportunitiesConfig`:

```rust
impl CreativeOpportunitiesConfig {
    /// Pre-compile glob patterns for all slots. Call once after deserialization.
    pub fn compile_slots(&mut self) {
        for slot in &mut self.slot {
            slot.compile_patterns();
        }
    }
}
```

- [ ] **Step 4: Wire slot compilation into `Settings::prepare_runtime`**

Glob pattern pre-compilation must happen once at startup, not per-request. `Settings::prepare_runtime` is already called after deserialization in both `from_toml_and_env` (build time) and `get_settings()` (runtime). Add slot compilation there:

```rust
// In settings.rs, inside Settings::prepare_runtime
pub fn prepare_runtime(&mut self) -> Result<(), Report<TrustedServerError>> {
    for handler in &self.handlers {
        handler.prepare_runtime()?;
    }
    // Pre-compile slot glob patterns for hot-path matching.
    if let Some(co) = &mut self.creative_opportunities {
        co.compile_slots();
    }
    Ok(())
}
```

Note: `prepare_runtime` must take `&mut self` for this change. Check current signature — if it takes `&self`, change it to `&mut self` and update call sites.

Also add a helper method for call sites that need the slot slice:

```rust
impl Settings {
    /// Returns compiled creative opportunity slots, or empty slice if disabled.
    pub fn creative_opportunity_slots(&self) -> &[CreativeOpportunitySlot] {
        self.creative_opportunities
            .as_ref()
            .map(|co| co.slot.as_slice())
            .unwrap_or(&[])
    }
}
```

- [ ] **Step 5: Update `build.rs` stub and slot validation**

First update the `creative_opportunities` stub in `build.rs` to add the `slot` field — without this the settings parse will fail at build time when `trusted-server.toml` contains `[[creative_opportunities.slot]]` entries:

```rust
mod creative_opportunities {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct CreativeOpportunitiesConfig {
        pub gam_network_id: String,
        #[serde(default)]
        pub auction_timeout_ms: Option<u32>,
        #[serde(default = "default_price_granularity")]
        pub price_granularity: String,
        // Use serde_json::Value to avoid pulling in full slot type in build context.
        #[serde(default)]
        pub slot: Vec<serde_json::Value>,
    }

    fn default_price_granularity() -> String {
        "dense".to_string()
    }
}
```

Then replace the separate-file validation block with reading slots from `Settings`:

```rust
// After settings are parsed, validate slot IDs
let slot_id_re = regex::Regex::new(r"^[A-Za-z0-9_\-]+$").expect("should compile regex");
if let Some(co) = &settings.creative_opportunities {
    for slot in &co.slot {
        if let Err(e) = trusted_server_core::creative_opportunities::validate_slot_id(&slot.id) {
            panic!("trusted-server.toml [creative_opportunities.slot]: {e}");
        }
    }
    if !co.slot.is_empty() {
        println!(
            "cargo:warning=creative_opportunities: {} slot(s) validated",
            co.slot.len()
        );
    }
}
```

Remove: `CREATIVE_OPPORTUNITIES_PATH` const, the `co_path.exists()` block, and the `println!("cargo:rerun-if-changed={}", CREATIVE_OPPORTUNITIES_PATH)` line.

Note: `build.rs` already pulls in `src/creative_opportunities.rs` as a module — make sure the module stub includes the new `Serialize` derive (it may need the serde `Serialize` import).

- [ ] **Step 6: Update `main.rs` — remove `SLOTS_FILE` static**

Remove:
```rust
const CREATIVE_OPPORTUNITIES_TOML: &str = include_str!("../../../creative-opportunities.toml");
static SLOTS_FILE: std::sync::LazyLock<...> = ...;
```

Replace `slots_file` parameter threading with deriving slots from `settings`:

Where `slots_file` was passed as `&*SLOTS_FILE`, pass `settings.creative_opportunity_slots()` instead. This requires `settings` to be available at that call site (it is — `settings` is already in scope).

Update function signatures in `main.rs` that reference `CreativeOpportunitiesFile` to accept `&[CreativeOpportunitySlot]` instead.

- [ ] **Step 7: Update `publisher.rs` function signatures**

Functions that take `&crate::creative_opportunities::CreativeOpportunitiesFile` change to `&[crate::creative_opportunities::CreativeOpportunitySlot]`:

```rust
// Before
pub(crate) fn handle_page_bids(
    ...
    slots_file: &crate::creative_opportunities::CreativeOpportunitiesFile,
    ...
)

// After
pub(crate) fn handle_page_bids(
    ...
    slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    ...
)
```

Inside the function body, replace `slots_file.slots` with `slots`.

Update all call sites and test helpers in `publisher.rs` that construct `CreativeOpportunitiesFile { slots: vec![...] }` to pass `&[slot]` directly.

- [ ] **Step 8: Update `trusted-server.toml`**

Move the slots from `creative-opportunities.toml` into `trusted-server.toml` under `[creative_opportunities]`. Use `[[creative_opportunities.slot]]` syntax. Use only example/fictional values per project convention (example.com domains, fictional IDs):

```toml
[creative_opportunities]
gam_network_id = "88059007"
auction_timeout_ms = 1500
price_granularity = "dense"

[[creative_opportunities.slot]]
id = "atf_sidebar_ad"
gam_unit_path = "/a/b/news"
div_id = "div-ad-atf-sidebar"
page_patterns = ["/news/**"]
formats = [{ width = 300, height = 250 }]
floor_price = 0.50

[creative_opportunities.slot.targeting]
pos = "atf"
zone = "atfSidebar"

[creative_opportunities.slot.providers.aps]
slot_id = "aps-slot-atf-sidebar"
```

- [ ] **Step 9: Delete `creative-opportunities.toml`**

```bash
git rm creative-opportunities.toml
```

- [ ] **Step 10: Run tests**

```bash
cargo test --workspace
```

Expected: all tests pass. Fix any compile errors from the signature changes.

- [ ] **Step 11: Run clippy and fmt**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 12: Commit**

```bash
git add -p
git commit -m "Move slot templates from creative-opportunities.toml into trusted-server.toml"
```

---

## Task 2: Namespace `window.__ts*` globals under `window._ts`

**What:** All `window.__ts*` globals become properties on a single `window._ts` namespace object. Changes must be coordinated across three files: `gpt_bootstrap.js`, `index.ts`, and `publisher.rs`. Tests in `index.test.ts` must be updated too.

**Rename table:**

| Old global | New property | Notes |
|---|---|---|
| `window.__ts_ad_slots` | `window._ts.adSlots` | Array, set at head-open |
| `window.__ts_bids` | `window._ts.bids` | Object, set before `</body>` |
| `window.__tsAdInit` | `window._ts.adInit` | Function |
| `window.__tsPrevGptSlots` | `window._ts.prevGptSlots` | Array |
| `window.__tsServicesEnabled` | `window._ts.servicesEnabled` | Boolean |
| `window.__tsDivToSlotId` | `window._ts.divToSlotId` | Object |
| `window.__tsSpaHookInstalled` | `window._ts.spaHookInstalled` | Boolean |

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/js/lib/src/integrations/gpt/index.test.ts`
- Modify: `crates/js/lib/test/integrations/gpt/index.test.ts` (if exists)

**Steps:**

- [ ] **Step 1: Update `publisher.rs` injected scripts**

`build_ad_slots_script` generates the `<script>` injected at `<head>` open. Change:

```rust
// Before
format!("<script>window.__ts_ad_slots=JSON.parse(\"{}\");</script>", escaped)

// After — initialise _ts if absent, then set adSlots
format!("<script>(window._ts=window._ts||{{}}).adSlots=JSON.parse(\"{}\");</script>", escaped)
```

`build_bids_script` generates the script injected before `</body>`. Change:

```rust
// Before
format!(
    "<script>window.__ts_bids=JSON.parse(\"{}\");if(typeof window.__tsAdInit===\"function\")window.__tsAdInit();</script>",
    escaped
)

// After
format!(
    "<script>(window._ts=window._ts||{{}}).bids=JSON.parse(\"{}\");if(typeof window._ts.adInit===\"function\")window._ts.adInit();</script>",
    escaped
)
```

Note: `{{}}` is the Rust format-string escape for a literal `{}`.

Update any test assertions in `publisher.rs` that check for the old global names.

- [ ] **Step 2: Update `gpt_bootstrap.js`**

Replace all `window.__ts*` references. The bootstrap IIFE runs before the TS bundle, so it must initialise `window._ts` if absent:

```js
(function () {
  if (typeof window === "undefined") return;
  // Initialise namespace; adInit guard prevents double-install.
  var ts = (window._ts = window._ts || {});
  if (ts.adInit) return;

  ts.adInit = function () {
    var slots = ts.adSlots || [];
    var bids = ts.bids || {};
    var divToSlotId = {};
    googletag.cmd.push(function () {
      var newSlots = [];
      slots.forEach(function (slot) {
        var s = googletag.defineSlot(slot.gam_unit_path, slot.formats, slot.div_id);
        if (!s) return;
        s.addService(googletag.pubads());
        Object.entries(slot.targeting || {}).forEach(function (e) {
          s.setTargeting(e[0], e[1]);
        });
        var b = bids[slot.id] || {};
        ["hb_pb", "hb_bidder", "hb_adid"].forEach(function (k) {
          if (b[k]) s.setTargeting(k, b[k]);
        });
        s.setTargeting("ts_initial", "1");
        divToSlotId[slot.div_id] = slot.id;
        newSlots.push(s);
      });
      ts.prevGptSlots = newSlots;
      ts.divToSlotId = divToSlotId;
      if (!ts.servicesEnabled) {
        googletag.pubads().enableSingleRequest();
        googletag.enableServices();
        ts.servicesEnabled = true;
        googletag.pubads().addEventListener("slotRenderEnded", function (ev) {
          var divId = ev.slot.getSlotElementId();
          var slotId = (ts.divToSlotId || {})[divId];
          if (!slotId) return;
          var b = (ts.bids || {})[slotId] || {};
          var ourBidWon =
            !ev.isEmpty &&
            (b.hb_adid
              ? ev.slot.getTargeting("hb_adid")[0] === b.hb_adid
              : !!b.hb_bidder);
          if (ourBidWon) {
            if (b.nurl) navigator.sendBeacon(b.nurl);
            if (b.burl) navigator.sendBeacon(b.burl);
          }
        });
      }
      if (newSlots.length > 0) {
        googletag.pubads().refresh(newSlots);
      }
    });
  };
})();
```

- [ ] **Step 3: Update `index.ts` — rename `TsWindow` type**

Replace the `TsWindow` interface:

```typescript
type TsNamespace = {
  adSlots?: TsAdSlot[];
  bids?: Record<string, TsBidData>;
  adInit?: () => void;
  prevGptSlots?: GoogleTagSlot[];
  servicesEnabled?: boolean;
  divToSlotId?: Record<string, string>;
  spaHookInstalled?: boolean;
};

type TsWindow = Window & {
  _ts?: TsNamespace;
};
```

- [ ] **Step 4: Update `installTsAdInit` in `index.ts`**

Change every `w.__ts*` access to `w._ts.*`. Initialise `w._ts` at function entry:

```typescript
export function installTsAdInit(): void {
  const w = window as TsWindow;
  const ts = (w._ts = w._ts ?? {});
  ts.adInit = function () {
    const slots = ts.adSlots ?? [];
    const bids = ts.bids ?? {};
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd?.push(() => {
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        g.destroySlots?.(ts.prevGptSlots);
        ts.prevGptSlots = [];
      }
      const newSlots: GoogleTagSlot[] = [];
      const divToSlotId: Record<string, string> = {};

      slots.forEach((slot) => {
        const gptSlot = g.defineSlot?.(slot.gam_unit_path, slot.formats as Array<number | number[]>, slot.div_id);
        if (!gptSlot) return;
        gptSlot.addService(g.pubads!());
        Object.entries(slot.targeting ?? {}).forEach(([k, v]) => gptSlot.setTargeting(k, v));
        const bid = bids[slot.id] ?? {};
        (['hb_pb', 'hb_bidder', 'hb_adid'] as const).forEach((key) => {
          if (bid[key]) gptSlot.setTargeting(key, bid[key]!);
        });
        gptSlot.setTargeting('ts_initial', '1');
        divToSlotId[slot.div_id] = slot.id;
        newSlots.push(gptSlot);
      });

      ts.prevGptSlots = newSlots;
      ts.divToSlotId = divToSlotId;

      if (!ts.servicesEnabled) {
        g.pubads!().enableSingleRequest();
        g.enableServices?.();
        ts.servicesEnabled = true;
        g.pubads!().addEventListener?.('slotRenderEnded', (event: SlotRenderEndedEvent) => {
          const divId: string = event.slot?.getSlotElementId?.() ?? '';
          const slotId = (ts.divToSlotId ?? {})[divId];
          if (!slotId) return;
          const bid = (ts.bids ?? {})[slotId] ?? {};
          const ourBidWon =
            !event.isEmpty &&
            (bid.hb_adid
              ? event.slot?.getTargeting?.('hb_adid')?.[0] === bid.hb_adid
              : !!bid.hb_bidder);
          if (ourBidWon) {
            if (bid.nurl) navigator.sendBeacon(bid.nurl);
            if (bid.burl) navigator.sendBeacon(bid.burl);
          }
        });
      }
      if (newSlots.length > 0) {
        g.pubads!().refresh(newSlots);
      }
    });
  };
}
```

- [ ] **Step 5: Update `installSpaHook` in `index.ts`**

Replace `__tsSpaHookInstalled` and `__ts_ad_slots`/`__ts_bids` reads:

```typescript
export function installSpaHook(): void {
  const win = window as TsWindow;
  const ts = (win._ts = win._ts ?? {});
  if (ts.spaHookInstalled) return;
  ts.spaHookInstalled = true;
  // ... rest of SPA hook logic uses ts.adSlots, ts.bids, ts.adInit
}
```

- [ ] **Step 6: Update tests in `index.test.ts`**

Find all test assertions that reference `window.__ts_ad_slots`, `window.__ts_bids`, `window.__tsAdInit`, etc. and update to `window._ts.adSlots`, `window._ts.bids`, `window._ts.adInit` etc.

Run tests first to see what fails:

```bash
cd crates/js/lib && npx vitest run
```

Fix each failing assertion.

- [ ] **Step 7: Run JS tests and format**

```bash
cd crates/js/lib && npx vitest run
cd crates/js/lib && npm run format
```

Expected: all tests pass, no format errors.

- [ ] **Step 8: Run Rust tests**

```bash
cargo test --workspace
```

Update any test assertions in `publisher.rs` that check for old global names (e.g. `script.contains("window.__ts_ad_slots")`).

- [ ] **Step 9: Run clippy and fmt**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 10: Commit**

```bash
git commit -m "Namespace window globals under window._ts"
```

---

## Task 3: Fix `formats` type and extract `ts_initial` constant

**What:** Two small TypeScript/JS cleanups. `TsAdSlot.formats` should be typed as `Array<[number, number]>` (tuple, not array-of-array) to match GPT's actual input. The string `'ts_initial'` is hardcoded in both `gpt_bootstrap.js` and `index.ts` — extract as a named constant in `index.ts` (no JS equivalent needed since the bootstrap is vanilla JS).

**Files:**
- Modify: `crates/js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js` (comment only — JS can't share TS constants)

**Steps:**

- [ ] **Step 1: Fix `TsAdSlot.formats` type**

In `index.ts`, change:

```typescript
// Before
interface TsAdSlot {
  ...
  formats: Array<number[]>;
}

// After
interface TsAdSlot {
  ...
  formats: Array<[number, number]>;
}
```

Update the cast at the GPT `defineSlot` call site — `[number, number]` satisfies `number | number[]` so the cast can be removed or simplified:

```typescript
// Before
slot.formats as Array<number | number[]>

// After — [number, number][] already satisfies Array<number | number[]>
slot.formats
```

- [ ] **Step 2: Extract `ts_initial` constant in `index.ts`**

Near the top of `index.ts`, add:

```typescript
const TS_INITIAL_TARGETING_KEY = 'ts_initial';
```

Replace both occurrences of `'ts_initial'` in `installTsAdInit` with `TS_INITIAL_TARGETING_KEY`.

Add a comment in `gpt_bootstrap.js` where `'ts_initial'` appears:

```js
// Keep in sync with TS_INITIAL_TARGETING_KEY in index.ts
s.setTargeting("ts_initial", "1");
```

- [ ] **Step 3: Run JS tests and format**

```bash
cd crates/js/lib && npx vitest run
cd crates/js/lib && npm run format
```

- [ ] **Step 4: Commit**

```bash
git commit -m "Fix TsAdSlot formats type and extract ts_initial constant"
```

---

## Final verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cd crates/js/lib && npx vitest run`
- [ ] `cd crates/js/lib && npm run format`
- [ ] `cd docs && npm run format`
