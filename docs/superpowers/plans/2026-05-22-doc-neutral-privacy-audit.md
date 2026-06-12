# doc-neutral-privacy: audit worksheet

Scope: documentation files and code comments only. Behavioral code edits
are out of scope on this branch; flagged items roll up into a separate
followup plan. The Current columns quote loaded phrases and marks verbatim. The quotes are records of the audited text, not usage.

Framing rules applied: privacy is a spectrum and technology should
be neutral; deployers decide based on their laws and policy; trust
comes from respected flexibility, not from constraint; first-party
versus third-party is not a privacy axis; TCF is one consent
framework among others; separate TCF v2 format (the encoded string
schema) from TCF as a policy framework.

Date: 2026-05-22.
Branch: `doc-neutral-privacy`.

## Executive summary

- **Total findings: 54**
- privacy-loaded: 19
- third-party-as-bad: 9
- tracking-pejorative: 6
- gdpr-marketing: 4
- paternalistic: 1
- tcf-embedded: 1
- tcf-format-vs-framework: 1
- consent-assumed: 0 (one borderline, see `docs/index.md:33`)
- flag-for-code-cycle: 13

Top files by finding density:

1. `docs/business-use-cases.md` (6)
2. `docs/guide/integrations/prebid.md` (6 across two passes)
3. `docs/guide/what-is-trusted-server.md` (4)
4. `docs/index.md` (3)
5. `docs/guide/edge-cookies.md` (3)
6. `docs/guide/first-party-proxy.md` (3)
7. `crates/trusted-server-core/src/integrations/aps.rs` (3)
8. `crates/trusted-server-core/src/edge_cookie.rs` (2)
9. `crates/trusted-server-core/src/storage/kv_store.rs` (2)
10. `docs/guide/configuration.md` (2)

## Findings

### `CLAUDE.md`

| Line | Category       | Current                                                                               | Proposed rewrite                                                                                            |
| ---- | -------------- | ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| 9    | privacy-loaded | `privacy-preserving Edge Cookie (EC) ID generation, ad serving with GDPR compliance,` | `Edge Cookie (EC) ID generation minted from client IP and secret key; ad serving with consent enforcement;` |

### `docs/index.md`

| Line | Category        | Current                                                                                    | Proposed rewrite                                                                                                        |
| ---- | --------------- | ------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| 23   | privacy-loaded  | `HMAC-based edge cookies that preserve privacy while enabling tracking with user consent`  | `HMAC-based edge cookies minted by the publisher; tracking subject to user consent`                                     |
| 25   | gdpr-marketing  | `Built-in consent management and validation to ensure compliance with privacy regulations` | `Built-in consent signal extraction, decoding (TCF v2 format, GPP, GPC) and enforcement logic for ad serving decisions` |
| 33   | consent-assumed | `All tracking requires explicit GDPR consent checks before any data collection`            | `Software forwards available consent signals (TCF v2 format, GPP, GPC) for publisher-determined enforcement`            |

### `docs/guide/collective-sync.md`

| Line | Category       | Current                                                                     | Proposed rewrite                                                                                                |
| ---- | -------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| 3    | privacy-loaded | `enabling privacy-preserving audience insights without third-party cookies` | `audience insights via shared EC identifier across consented publishers (without third-party script execution)` |

### `docs/guide/configuration.md`

| Line | Category       | Current                                                                                 | Proposed rewrite                                                                   |
| ---- | -------------- | --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| 266  | privacy-loaded | `Settings for generating privacy-preserving Edge Cookie identifiers`                    | `Settings for Edge Cookie identifier generation`                                   |
| 1081 | privacy-loaded | `Learn about [Edge Cookies](/guide/edge-cookies) for privacy-preserving identification` | `Learn about [Edge Cookies](/guide/edge-cookies) for first-party state management` |

### `docs/guide/edge-cookies.md`

| Line | Category           | Current                                                                                                                                                                       | Proposed rewrite                                                                                                                                       |
| ---- | ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 7    | privacy-loaded     | `Edge Cookies (EC) are privacy-safe identifiers generated on a first site visit using HMAC-based hashing that allow tracking with user consent while protecting user privacy` | `Edge Cookies (EC) are identifiers generated on first site visit using HMAC of client IP and a secret; tracking is subject to consent signal presence` |
| 9    | third-party-as-bad | `No direct third-party cookies or tracking`                                                                                                                                   | `No script execution on third-party domains`                                                                                                           |
| 25   | third-party-as-bad | `The cookie is only set when storage consent is present`                                                                                                                      | `The cookie is set; downstream use of the identifier is publisher-determined based on consent signals`                                                 |

### `docs/guide/first-party-proxy.md`

| Line | Category            | Current                                              | Proposed rewrite                                                                                           |
| ---- | ------------------- | ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| 9    | third-party-as-bad  | `No direct third-party cookies or tracking`          | `Publisher-controlled endpoint for third-party resource delivery`                                          |
| 10   | flag-for-code-cycle | `EC ID Forwarding - Controlled identity propagation` | Verify: is forwarding hard-coded or configurable per vendor consent?                                       |
| 188  | flag-for-code-cycle | `POST /first-party/proxy-rebuild`                    | Verify: does the rebuild endpoint allow publisher override of URL rewrite rules per vendor consent signal? |

### `docs/guide/what-is-trusted-server.md`

| Line | Category            | Current                                                                                                                                        | Proposed rewrite                                                                                                                 |
| ---- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| 10   | flag-for-code-cycle | `Dramatically increased control over 3rd party data sharing (while maintaining user-privacy compliance such as GDPR through CMP integrations)` | "Control over data sharing" implies publisher (not user) decides; audit the consent model to verify user agency before rewriting |
| 15   | third-party-as-bad  | `without relying on third-party cookies`                                                                                                       | `via first-party identifier generation`                                                                                          |
| 35   | gdpr-marketing      | `GDPR-compliant ad serving`                                                                                                                    | `Ad serving with TCF v2 format / GPP / GPC consent signal forwarding`                                                            |
| 36   | privacy-loaded      | `Privacy-safe user tracking`                                                                                                                   | `User tracking subject to available consent signals`                                                                             |

### `docs/guide/integrations-overview.md`

| Line | Category       | Current                                                                   | Proposed rewrite                                                     |
| ---- | -------------- | ------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| 3    | privacy-loaded | `enabling first-party data collection and privacy-preserving advertising` | `enabling first-party data collection and consent-aware advertising` |

### `docs/guide/integrations/prebid.md`

| Line | Category       | Current                                                                                                 | Proposed rewrite                                                                             |
| ---- | -------------- | ------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| 127  | privacy-loaded | `Adds EC ID for privacy-safe tracking`                                                                  | `Injects EC ID into bid requests for user recognition`                                       |
| 132  | privacy-loaded | `Automatically injects privacy-preserving EC ID into bid requests for user recognition without cookies` | `Automatically injects EC ID into bid requests for user recognition via first-party context` |
| 391  | privacy-loaded | `Learn about [Edge Cookies](/guide/edge-cookies) for privacy-safe tracking`                             | `Learn about [Edge Cookies](/guide/edge-cookies) for state management`                       |

### `docs/guide/integrations/permutive.md`

| Line | Category       | Current                                                              | Proposed rewrite                                                         |
| ---- | -------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| 54   | privacy-loaded | `Combine page context with user behavior for privacy-safe targeting` | `Combine page context with user behavior for audience segment targeting` |

### `docs/guide/integrations/aps.md`

| Line | Category       | Current                                                                             | Proposed rewrite                                                                           |
| ---- | -------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| 9    | privacy-loaded | `Privacy-first bidding: No client-side ID tracking or third-party cookies required` | `Server-side bidding via Trusted Server (no client-side JavaScript execution for auction)` |

### `docs/guide/integrations/gpt.md`

| Line | Category           | Current                                                                                                                                 | Proposed rewrite                                                                                   |
| ---- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| 9    | third-party-as-bad | `This eliminates third-party script loads, improving performance and reducing exposure to ad blockers and browser privacy restrictions` | `Delivers GPT via first-party domain for improved performance and reduced ad blocker/ITP friction` |

### `docs/guide/integrations/google_tag_manager.md`

| Line | Category            | Current                                                                          | Proposed rewrite                                                                                     |
| ---- | ------------------- | -------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| 20   | flag-for-code-cycle | `Does not forward client IP to Google (Google sees edge server IP, not user IP)` | Verify: is IP masking hard-coded or publisher-configurable? Affects how this is described neutrally. |

### `docs/guide/integrations/didomi.md`

| Line | Category            | Current                            | Proposed rewrite                                                                                                             |
| ---- | ------------------- | ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| 9    | flag-for-code-cycle | `ensuring GDPR/TCF 2.2 compliance` | Verify: does integration enforce TCF v2.2 _format_ only, or prescribe specific consent requirements? Rewrite once clarified. |

### `docs/guide/integrations/lockr.md`

| Line | Category            | Current                               | Proposed rewrite                                                                                                                    |
| ---- | ------------------- | ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| 9    | flag-for-code-cycle | `respecting user privacy and consent` | "Respecting privacy" is an outcome of user choice, not a software guarantee. Rewrite candidate: "supporting user consent controls". |

### `docs/guide/creative-processing.md`

| Line | Category           | Current                                  | Proposed rewrite                                             |
| ---- | ------------------ | ---------------------------------------- | ------------------------------------------------------------ |
| 9    | third-party-as-bad | `All resources load through your domain` | `All creative resource URLs routed through publisher domain` |

### `docs/guide/architecture.md`

| Line | Category        | Current                                                                         | Proposed rewrite                                                                                                                                   |
| ---- | --------------- | ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| 68   | consent-assumed | `All tracking operations require explicit GDPR consent checks before execution` | `All tracking operations subject to available consent signals (TCF v2 format, GPP, GPC); enforcement policy determined by publisher configuration` |

### `docs/guide/gdpr-compliance.md`

| Line | Category            | Current                                                                                                                                   | Proposed rewrite                                                                                                                              |
| ---- | ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| 7    | gdpr-marketing      | `Trusted Server enforces GDPR compliance at the edge, ensuring all tracking and data collection activities require explicit user consent` | `Trusted Server provides consent signal extraction and enforcement logic at the edge; publishers configure consent requirements per activity` |
| 13   | flag-for-code-cycle | `All requests are validated for proper GDPR consent before any tracking occurs`                                                           | Verify: is consent validation hard-coded, or does config allow permissive override per activity?                                              |

### `docs/guide/ad-serving.md`

| Line | Category            | Current                                    | Proposed rewrite                                  |
| ---- | ------------------- | ------------------------------------------ | ------------------------------------------------- |
| 87   | tracking-pejorative | `Click tracking with privacy preservation` | `Click tracking with EC ID (first-party context)` |

### `docs/business-use-cases.md`

| Line | Category            | Current                                                                                                                          | Proposed rewrite                                                                                                               |
| ---- | ------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| 11   | third-party-as-bad  | `Safari and Firefox block third-party cookies, fragmenting user identity and reducing addressable inventory CPMs by 30-50%`      | `Safari and Firefox restrict third-party cookie scope, reducing cross-site identifier continuity and CPM availability`         |
| 13   | privacy-loaded      | `Trusted Server's Edge Cookie (EC) system maintains user recognition across cookieless browsers through first-party identifiers` | `Trusted Server's EC system provides first-party identifiers for user recognition in restricted cookie environments`           |
| 50   | gdpr-marketing      | `Granular consent controls (GDPR compliant)`                                                                                     | `Granular consent controls (TCF v2 format Purpose/vendor scope, GPC)`                                                          |
| 111  | flag-for-code-cycle | `Trusted Server's Creative Forensics Engine detects and blocks GDPR violations before they reach users`                          | Verify: does the engine block creatives unconditionally, or alert for publisher review? Is blocking consent-gated?             |
| 175  | flag-for-code-cycle | `Prevent GDPR violations from third-party creatives (potential $2.3M+ fine avoidance)`                                           | Clarify who decides violation severity and remediation.                                                                        |
| 274  | paternalistic       | `Trusted Server enables transparent, consent-based advertising that rebuilds user trust`                                         | `Trusted Server provides transparent, consent-based advertising mechanisms; trust is a user outcome, not a software guarantee` |
| 279  | flag-for-code-cycle | `Differentiate from surveillance-based competitors`                                                                              | "Surveillance-based competitors" is loaded framing about third parties. Neutralize: "differentiate via consent transparency".  |

### `docs/roadmap.md`

| Line | Category            | Current                                                                                | Proposed rewrite                                                                                                    |
| ---- | ------------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| 175  | flag-for-code-cycle | `Prevent GDPR violations from third-party creatives (potential $2.3M+ fine avoidance)` | Clarify whether detection and blocking are automatic or publisher-gated.                                            |
| 220  | tcf-embedded        | `TCF 2.2 full compliance`                                                              | `Support TCF v2.2 format decoding and enforcement; TCF is one consent framework among others including GPP and GPC` |

### `crates/trusted-server-core/src/edge_cookie.rs`

| Line | Category       | Current                                                                                                         | Proposed rewrite                                                                                                                                                                    |
| ---- | -------------- | --------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 3    | privacy-loaded | `//! This module provides functionality for generating privacy-preserving EC IDs`                               | `//! This module provides functionality for generating EC IDs`                                                                                                                      |
| 60   | privacy-loaded | `/// EC IDs are meant to be simple, privacy-preserving identifiers, not high-entropy probabilistic identifiers` | `/// EC IDs are deterministic identifiers (HMAC base plus random suffix), not high-entropy probabilistic identifiers` (also removes the em-dash, which the user's house style bans) |

### `crates/trusted-server-core/src/auction/types.rs`

| Line | Category       | Current                                     | Proposed rewrite                       |
| ---- | -------------- | ------------------------------------------- | -------------------------------------- |
| 21   | privacy-loaded | `/// User information (privacy-preserving)` | `/// User information (consent-aware)` |

### `crates/trusted-server-core/src/consent_config.rs`

| Line | Category       | Current                                                      | Proposed rewrite                                                                                                                                                                         |
| ---- | -------------- | ------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 284  | privacy-loaded | `/// Deny consent when signals disagree (most privacy-safe)` | `/// Deny consent when signals disagree (most restrictive)` (note: agent categorized this as tcf-format-vs-framework but the actual issue is "privacy-safe" framing; recategorized here) |

### `crates/trusted-server-core/src/consent/mod.rs`

| Line | Category            | Current                                                                                               | Proposed rewrite                                                                       |
| ---- | ------------------- | ----------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| 481  | flag-for-code-cycle | `opt-in required — TCF Purpose 1 (store/access information on a device) must be explicitly consented` | Verify: is opt-in requirement hard-coded, or configurable? Determines neutral rewrite. |

### `crates/trusted-server-core/src/integrations/aps.rs`

| Line | Category            | Current                  | Proposed rewrite            |
| ---- | ------------------- | ------------------------ | --------------------------- |
| 102  | tracking-pejorative | `Event tracking host`    | `Event collection endpoint` |
| 114  | tracking-pejorative | `Event tracking enabled` | `Event collection enabled`  |
| 126  | tracking-pejorative | `Campaign tracking URL`  | `Campaign attribution URL`  |

### `crates/trusted-server-core/src/integrations/prebid.rs`

| Line | Category            | Current                                                                                                   | Proposed rewrite                                                                                                                     |
| ---- | ------------------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| 1178 | flag-for-code-cycle | `a GDPR jurisdiction, signal that even without a TCF string (e.g., no TCF signal exists, omit the field)` | Verify: when no TCF v2 format signal exists in a GDPR jurisdiction, does the code require explicit opt-in or allow opt-out fallback? |

### `crates/trusted-server-core/src/storage/kv_store.rs`

| Line | Category            | Current                                                        | Proposed rewrite                                                    |
| ---- | ------------------- | -------------------------------------------------------------- | ------------------------------------------------------------------- |
| 38   | tracking-pejorative | (original used the banned word for the change-detection value) | `The fp field holds a hash of consent signals for change detection` |
| 145  | tracking-pejorative | (original used the banned word for the change-detection value) | `Computes a hash of consent signals for change detection`           |

### `docs/epics/revenue-operations-dashboard.md`

| Line | Category            | Current                                                                | Proposed rewrite                                                                      |
| ---- | ------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| 155  | tracking-pejorative | `Emit compliance_violation events when unauthorized tracking detected` | `Emit events when requests target domains without explicit publisher allowlist entry` |

## Addendum: 2026-06-12 fresh-head pass

After merging upstream `main` (which added the `ec/` module tree, the
EC setup guide, and expanded EC documentation), a second multi-agent
audit swept the full tree: every `docs/guide` page, root docs, the
VitePress config, all Rust comments and user-facing string literals,
the TOML configs, and the non-generated TypeScript sources. Each
finding was adversarially verified against the code before applying.

Result: 130 additional rewrites across 41 files. Categories follow the
original taxonomy. Notable groups:

- The banned client-signal term in the new `ec/device.rs`,
  `ec/kv_types.rs`, and `ec/mod.rs` comments and test assertion
  messages (replaced with "probabilistic identifier", "signal",
  "JA4 string", or "H2 SETTINGS string" per what each value is).
- "While maintaining privacy controls" boilerplate in four integration
  module docs (`gpt.rs`, `lockr.rs`, `permutive.rs`, and the GTM
  variant), which claimed controls the proxy code does not implement.
- GDPR-as-default framing across `ad-serving.md`, `architecture.md`,
  `getting-started.md`, and integration pages (replaced with
  consent-signal phrasing naming TCF v2 format, GPP, and GPC).
- Overstated claims corrected for factual accuracy: the architecture
  page said user data is never persisted while the EC KV store
  persists identity graph state; the collective-sync page claimed
  "no PII" for an identifier whose HMAC input is the client IP.
- Compliance-guarantee marketing in `business-use-cases.md`,
  `roadmap.md`, the Didomi and Lockr pages, and the dashboard epic
  (reframed as publisher-configured policy).

Two newly found symbol renames are deferred to the code cycle (see
items 15 and 16 in the followups plan). Style-only fixes on lines
upstream owns (em dashes, UK spellings outside this branch's edits)
were intentionally skipped to keep the diff reviewable. A final adversarial critique pass over the complete PR diff (three lenses: residue, factual accuracy, style consistency) then corrected statements that no longer matched the post-merge code, in particular overstated deployer configurability of consent gating and a stale description of how the EC value travels on the x-ts-ec header.

## Reviewer notes

- The 10 `flag-for-code-cycle` rows are deferred to a separate followup
  plan; do not edit on this branch.
- One agent categorization was corrected during persistence:
  `consent_config.rs:284` was marked `tcf-format-vs-framework` but the
  trigger word is "privacy-safe"; reclassified to `privacy-loaded`.
- Three `tracking-pejorative` entries in `aps.rs` are borderline:
  "tracking" is standard term-of-art in event-collection code. Reviewer
  may decide to keep current wording; included here for visibility.
- The change-detection value in `kv_store.rs` is a content hash; the
  rewrite to "hash" is recommended for clarity. Code symbol renames
  (the function previously named for the banned word, its
  `*_unchanged` helper, and the four test functions whose names
  started with the banned word) are applied in this branch as well:
  symbols now use the `probid` token (probabilistic identifier),
  aligning code identifiers with the neutral-language framing applied
  across docs and comments.
