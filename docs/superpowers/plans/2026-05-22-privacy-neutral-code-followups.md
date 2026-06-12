# doc-neutral-privacy: code-cycle followups

Items extracted from `2026-05-22-doc-neutral-privacy-audit.md` that
cannot be addressed by a documentation or comment rewrite alone. Each
row points at a behavior the documentation describes that may be
hard-coded against one consent regime or one jurisdiction. Neutral
rewording would either misrepresent the current code (if the code is
hard-coded) or be premature (if the code is already configurable and
the docs simply need to say so).

Resolve each item by checking the code, then doing one of:

1. **Rewrite the doc** if the code already supports deployer choice and
   the doc just over-states the constraint.
2. **Make the behavior configurable in the code** if the constraint
   was assumed-by-design and we want to give deployers the flexibility
   to choose, then rewrite the doc to describe the new configuration
   surface.
3. **Leave the behavior as-is** if the constraint is intentional and
   load-bearing, and rewrite the doc to be honest about that.

This branch (`doc-neutral-privacy`) does not edit code. These items
move into a separate cycle.

## Framing for the eventual code-cycle PR

Four propositions to apply:

1. Privacy is a spectrum, not a binary.
2. Technology should be neutral.
3. Deployers decide based on their laws and policy.
4. Trust comes from respected flexibility, not from constraint.

For each item below, the question is whether the current implementation
holds those propositions or not.

## Items

### 1. EC ID forwarding to proxied endpoints

- **Where it shows up**: `docs/guide/first-party-proxy.md:10`.
- **Doc currently says**: "EC ID Forwarding - Controlled identity propagation".
- **Question to answer**: Is EC ID forwarding hard-coded for every proxied vendor, or is it gated per-vendor by consent signals or deployer config?
- **Likely action**:
  - If always-on: document plainly that it is always-on, no claim that it is "controlled".
  - If per-vendor configurable: document the configuration surface so deployers can see and change it.
  - If consent-gated only for some signals (for example TCF v2 format Purpose 1): document the gate and the override path.

### 2. First-party proxy rebuild endpoint

- **Where it shows up**: `docs/guide/first-party-proxy.md:188`.
- **Doc currently says**: "POST /first-party/proxy-rebuild" with no detail on who controls the rewrite rules.
- **Question to answer**: Can the deployer override URL rewrite rules per vendor or per consent signal at this endpoint, or does the rebuild always apply a global ruleset?
- **Likely action**: Document the actual override surface (if any) so deployers can see the mechanism. If none, document that and flag whether the code should grow one.

### 3. "Control over data sharing" framing

- **Where it shows up**: `docs/guide/what-is-trusted-server.md:10`.
- **Doc currently says**: "Dramatically increased control over 3rd party data sharing (while maintaining user-privacy compliance such as GDPR through CMP integrations)".
- **Question to answer**: Is the "control" landing with the user (consumer sovereignty) or with the publisher? The wording is ambiguous.
- **Likely action**: Trace the actual consent and data-sharing decision points. The neutral rewrite differs depending on whether the agent is the user (with publisher tools) or the publisher (with user inputs).

### 4. Google Tag Manager IP masking

- **Where it shows up**: `docs/guide/integrations/google_tag_manager.md:20`.
- **Doc currently says**: "Does not forward client IP to Google (Google sees edge server IP, not user IP)".
- **Question to answer**: Is IP masking hard-coded for this integration, or can the deployer configure forwarding when their policy or contract permits?
- **Likely action**: If hard-coded, document that the behavior is intentional and not configurable; if configurable, document the toggle so deployers can choose.

### 5. Didomi integration scope

- **Where it shows up**: `docs/guide/integrations/didomi.md:9`.
- **Doc currently says**: "ensuring GDPR/TCF 2.2 compliance".
- **Question to answer**: Does the integration enforce the TCF v2.2 format (a schema concern) or does it prescribe a policy posture (a regulatory concern)? The wording conflates the two.
- **Likely action**: Separate the format support (always present, technical) from the policy assumption (deployer choice). Likely a pure doc fix once the question is answered, but logged as a code-cycle item in case the code does prescribe more than format handling.

### 6. Lockr "respecting user privacy and consent"

- **Where it shows up**: `docs/guide/integrations/lockr.md:9`.
- **Doc currently says**: "respecting user privacy and consent".
- **Question to answer**: What does the integration actually do at the consent boundary? Is there a hard-coded check, an optional check, or a passthrough?
- **Likely action**: "Supporting user consent controls" is a candidate doc rewrite; confirm it matches code behavior before applying. If the code does more or less than that, change to match.

### 7. Global "all requests validated for consent" claim

- **Where it shows up**: `docs/guide/gdpr-compliance.md:13`.
- **Doc currently says**: "All requests are validated for proper GDPR consent before any tracking occurs".
- **Question to answer**: Is consent validation hard-coded on every tracking path, or does config permit permissive operation in some deployments? "All requests" is a strong claim.
- **Likely action**: If hard-coded, narrow the doc to which paths and what "tracking" means in code terms. If configurable, document the configuration knobs so deployers can see the choices available to them.

### 8. Creative Forensics Engine blocking

- **Where it shows up**: `docs/business-use-cases.md:111`.
- **Doc currently says**: "Trusted Server's Creative Forensics Engine detects and blocks GDPR violations before they reach users".
- **Question to answer**: Does the engine block creatives unconditionally? Or alert the deployer for review? Is the blocking gated by consent or by deployer configuration?
- **Likely action**: This wording will not survive any of the four propositions in the PR framing. Decide whether the engine should block, alert, or both, then document plainly.

### 9. "GDPR violations" framing in business material

- **Where it shows up**: `docs/business-use-cases.md:175` and `docs/roadmap.md:175` (duplicate framing).
- **Doc currently says**: "Prevent GDPR violations from third-party creatives (potential $2.3M+ fine avoidance)".
- **Question to answer**: Who decides what constitutes a violation? Is the determination automated or human? Is fine-avoidance language one we want to keep, or is it the kind of compliance marketing the PR framing rules out?
- **Likely action**: Both lines need rewriting; the question is whether the implementation supports the rewrite or also needs adjustment.

### 10. "Surveillance-based competitors" language

- **Where it shows up**: `docs/business-use-cases.md:279`.
- **Doc currently says**: "Differentiate from surveillance-based competitors".
- **Question to answer**: This is loaded framing about third parties that the neutral-framing rules reject. Rewriting the doc is straightforward; flagging here because it is a marketing-message change that may need product-marketing sign-off before edit.
- **Likely action**: Replace with "differentiate via consent transparency" or similar; route past whoever owns the business-use-cases page before applying.

### 11. TCF v2 Purpose 1 opt-in requirement

- **Where it shows up**: `crates/trusted-server-core/src/consent/mod.rs:481` (and surrounding logic).
- **Comment currently says**: "opt-in required, TCF Purpose 1 (store/access information on a device) must be explicitly consented".
- **Question to answer**: Is this opt-in requirement hard-coded, or configurable per deployer? Deployers in jurisdictions that recognize legitimate interest or opt-out models may want a different default.
- **Likely action**: If configurable, document the knob and reword the comment to point at it; if hard-coded, decide whether to add a configuration surface.

### 12. Prebid GDPR jurisdiction handling without a TCF signal

- **Where it shows up**: `crates/trusted-server-core/src/integrations/prebid.rs:1178`.
- **Comment currently says**: paraphrased, "in a GDPR jurisdiction, signal that even without a TCF string (no TCF signal exists, omit the field)".
- **Question to answer**: When no TCF v2 format signal exists in a GDPR jurisdiction, does the code require explicit opt-in, default to opt-out, or pass the request through without a signal? Each posture is a deployer choice; the comment suggests an opinionated default.
- **Likely action**: Confirm the actual default. If configurable, document the configuration. If hard-coded, decide whether to add a configuration surface or surface the assumption clearly.

### 13. Jurisdictional binding in "all tracking" claim

- **Where it shows up**: `docs/guide/architecture.md:68`.
- **Doc currently says**: "All tracking operations require explicit GDPR consent checks before execution".
- **Question to answer**: This is the architecture page; the wording assumes GDPR as the operating jurisdiction. Is the code actually jurisdiction-aware (per memory rule on deployer choice), or does it treat GDPR as the universal regime?
- **Likely action**: A doc rewrite to "All tracking operations subject to available consent signals (TCF v2 format, GPP, GPC); enforcement policy determined by publisher configuration" works if the code is jurisdiction-aware. If the code treats GDPR as universal, that is itself a code-cycle item: deployers operate under different laws and should be able to configure accordingly.

### 14. JA4 endpoint description wording in client-facing surfaces

- **Where it shows up**: any future user-facing description of the `/_ts/debug/ja4` endpoint (currently only in `trusted-server.toml` comments, which have been rewritten in this cycle).
- **Question to answer**: When the endpoint is documented in `docs/guide/`, ensure the description follows the rule that JA4 / H2 / TLS values are described as "probabilistic identifiers", not the banned word.
- **Likely action**: Apply on any new doc page that mentions the endpoint.

## Suggested ordering for the code cycle

1. Items 11, 12, 13 first: they sit in the consent core and any
   answer there cascades into the doc rewrites for the other rows.
2. Items 1, 2, 4, 5, 6, 7: integration and proxy surface; each is
   small and can be resolved per-integration.
3. Items 3, 8, 9, 10, 14: marketing, business-use-case, and any new
   doc pages; rewrite driven, with the consent-core answer informing
   the framing.

## Handoff

When this branch lands, the followup cycle should reference both:

- this file (the deferred items and their open questions),
- `2026-05-22-doc-neutral-privacy-audit.md` (the full audit and the
  rewrites that did land on this branch).

The PR description for `doc-neutral-privacy` should mention that the
14 items in this file are intentionally deferred and that the next
cycle will resolve them.

## Addendum: 2026-06-12 fresh-head pass

A second audit pass ran after merging upstream `main` (which replaced
`edge_cookie.rs` and `storage/kv_store.rs` with the new `ec/` module
tree). Status changes to the items above:

- **Item 5 (Didomi)**: doc side resolved. `didomi.md:9` now reads
  "first-party context for Didomi's GDPR/TCF 2.2 consent flows".
- **Item 6 (Lockr)**: doc side resolved. `lockr.md` now describes
  forwarding as "subject to available consent signals".
- **Items 8 and 9 (Creative Forensics framing)**: doc side resolved.
  The business and roadmap pages now condition blocking on "the
  publisher's configured consent policy". The code question (block
  versus alert, and who configures it) remains open for the code cycle.
- **Item 10 (surveillance language)**: resolved in the original branch
  commit.
- **Item 11 (TCF Purpose 1 opt-in)**: line reference is stale after the
  upstream merge. The jurisdiction model now lives in
  `crates/trusted-server-core/src/consent/mod.rs` (`allows_ec_creation`)
  and `consent/jurisdiction.rs`. The question stands: GDPR opt-in,
  US state opt-out, and fail-closed defaults are hard-coded per
  jurisdiction rather than configurable.
- **Item 13 (architecture claim)**: doc side resolved. The code is
  jurisdiction-aware (GDPR, US state, non-regulated, unknown), so the
  rewrite applied on this branch is accurate.

New deferred items from the upstream `ec/` module (comments were
neutralized on this branch; symbol renames are code-cycle work):

### 15. Test function name in `ec/device.rs`

- **Where**: `crates/trusted-server-core/src/ec/device.rs`, the
  `looks_like_browser_unknown_*_still_passes` test near line 532,
  whose current name carries the banned client-signal term.
- **Likely action**: rename to
  `looks_like_browser_unknown_signals_still_passes` in the code cycle.

### 16. `h2_fp_hash` symbol family

- **Where**: `DeviceSignals.h2_fp_hash` and `compute_h2_fp_hash` in
  `ec/device.rs`, `KvDevice.h2_fp_hash` in `ec/kv_types.rs`, and the
  `h2_fp` label in the `/_ts/debug/ja4` response body.
- **Constraint**: `KvDevice.h2_fp_hash` is a serialized KV schema field
  and `h2_fp` is a documented debug-output label, so renaming either is
  a breaking change. A Rust-side rename would need
  `#[serde(rename = "h2_fp_hash")]` to preserve stored JSON.
- **Likely action**: probably keep as-is; the abbreviation is opaque
  enough that it does not carry the loaded term into prose.

### 17. `applies_in` doc comment accuracy

- **Where**: `crates/trusted-server-core/src/consent_config.rs`, the
  `GdprConfig` doc comment.
- **Question**: the comment says `applies_in` is used for observability
  and logging only, but `detect_jurisdiction` in
  `consent/jurisdiction.rs` uses the list to select the GDPR rules for
  EC creation gating. The comment under-states the list's effect.
- **Likely action**: correct the comment in the code cycle.

### Obsolete

The consent change-detection symbol renames from the original session
(the old names carried the banned word, the new names used the
`probid` token) were removed by the upstream merge along with
`storage/kv_store.rs`. No action remains.
