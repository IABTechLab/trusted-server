# Sourcepoint GPP Consent for Edge Cookie Generation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable EC generation for sites using Sourcepoint by mirroring localStorage consent into cookies (client) and recognizing GPP US `sale_opt_out` as a consent signal (server).

**Architecture:** New JS-only `sourcepoint` integration auto-discovers `_sp_user_consent_*` in localStorage and writes `__gpp` / `__gpp_sid` cookies. Server-side, `GppConsent` gains a `us_sale_opt_out: Option<bool>` field extracted from any GPP US section (IDs 7–23). `allows_ec_creation()` checks this field between the existing TCF and `us_privacy` branches.

**Tech Stack:** TypeScript (Vitest, jsdom), Rust (iab_gpp crate for GPP section decoding)

**Spec:** `docs/superpowers/specs/2026-04-15-sourcepoint-gpp-consent-design.md`

---

## File Map

| File                                                        | Action | Responsibility                                      |
| ----------------------------------------------------------- | ------ | --------------------------------------------------- |
| `crates/trusted-server-core/src/consent/types.rs`           | Modify | Add `us_sale_opt_out: Option<bool>` to `GppConsent` |
| `crates/trusted-server-core/src/consent/gpp.rs`             | Modify | Decode US sections, extract `sale_opt_out`          |
| `crates/trusted-server-core/src/consent/mod.rs`             | Modify | Add GPP US branch in `allows_ec_creation()`, tests  |
| `crates/js/lib/src/integrations/sourcepoint/index.ts`       | Create | localStorage auto-discovery, cookie mirroring       |
| `crates/js/lib/test/integrations/sourcepoint/index.test.ts` | Create | Vitest tests for cookie mirroring                   |

---

## Task 1: Add `us_sale_opt_out` field to `GppConsent`

**Files:**

- Modify: `crates/trusted-server-core/src/consent/types.rs:297-305`

- [ ] **Step 1: Add the field**

In `crates/trusted-server-core/src/consent/types.rs`, add `us_sale_opt_out` to `GppConsent`:

```rust
/// Decoded GPP (Global Privacy Platform) consent data.
///
/// Wraps the `iab_gpp` crate's decoded output with our domain types.
#[derive(Debug, Clone)]
pub struct GppConsent {
    /// GPP header version.
    pub version: u8,
    /// Active section IDs present in the GPP string.
    pub section_ids: Vec<u16>,
    /// Decoded EU TCF v2.2 section (if present in GPP, section ID 2).
    pub eu_tcf: Option<TcfConsent>,
    /// Whether the user opted out of sale of personal information via a US GPP
    /// section (IDs 7–23).
    ///
    /// - `Some(true)` — a US section is present and `sale_opt_out == OptedOut`
    /// - `Some(false)` — a US section is present and user did not opt out
    /// - `None` — no US section exists in the GPP string
    pub us_sale_opt_out: Option<bool>,
}
```

- [ ] **Step 2: Fix compilation — update all `GppConsent` construction sites**

There are existing places that construct `GppConsent`. Each needs the new field. Search for them:

In `crates/trusted-server-core/src/consent/gpp.rs` (~line 74), update `decode_gpp_string`:

```rust
    Ok(GppConsent {
        version: 1,
        section_ids,
        eu_tcf,
        us_sale_opt_out: None, // placeholder — Task 2 fills this in
    })
```

In `crates/trusted-server-core/src/consent/mod.rs`, find every test that constructs `GppConsent` (search for `GppConsent {`). Add `us_sale_opt_out: None` to each. There are instances around lines 720, 883, and 965:

```rust
    gpp: Some(GppConsent {
        version: 1,
        section_ids: vec![2],
        eu_tcf: Some(...),
        us_sale_opt_out: None,
    }),
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --workspace`
Expected: compiles with no errors.

- [ ] **Step 4: Run tests to confirm nothing broke**

Run: `cargo test --workspace`
Expected: all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/consent/types.rs \
       crates/trusted-server-core/src/consent/gpp.rs \
       crates/trusted-server-core/src/consent/mod.rs
git commit -m "Add us_sale_opt_out field to GppConsent"
```

---

## Task 2: Decode US sale opt-out from GPP sections

**Files:**

- Modify: `crates/trusted-server-core/src/consent/gpp.rs`

- [ ] **Step 1: Write the failing test for US sale opt-out extraction**

Add to the `#[cfg(test)] mod tests` block in `crates/trusted-server-core/src/consent/gpp.rs`:

```rust
    // A GPP string with UsNat section (section ID 7).
    // Header "DBABLA" encodes: version=1, section IDs=[7] (UsNat).
    // The section string encodes a UsNat v1 core with sale_opt_out=DidNotOptOut (2).
    #[test]
    fn decodes_us_sale_opt_out_not_opted_out() {
        // Build a real GPP string with UsNat section using iab_gpp parsing.
        // "DBABLA~BVQqAAAAAgA.QA" is the example from the issue (Sourcepoint payload).
        let result = decode_gpp_string("DBABLA~BVQqAAAAAgA.QA");
        match &result {
            Ok(gpp) => {
                assert_eq!(
                    gpp.us_sale_opt_out,
                    Some(false),
                    "should extract sale_opt_out=false from UsNat section"
                );
            }
            Err(e) => {
                // If the specific GPP string doesn't parse, test with section ID presence.
                // The important thing is that the decode_us_sale_opt_out function is wired up.
                panic!("GPP decode failed: {e}");
            }
        }
    }

    #[test]
    fn no_us_section_returns_none() {
        // GPP_TCF_AND_USP has section IDs [2, 6] — no US sections (7–23).
        let result = decode_gpp_string(GPP_TCF_AND_USP).expect("should decode GPP");
        assert_eq!(
            result.us_sale_opt_out, None,
            "should return None when no US section (7-23) is present"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --workspace -p trusted-server-core -- consent::gpp::tests::decodes_us_sale_opt_out`
Expected: FAIL — `us_sale_opt_out` is hardcoded to `None`.

- [ ] **Step 3: Implement `decode_us_sale_opt_out`**

In `crates/trusted-server-core/src/consent/gpp.rs`, add after `decode_tcf_from_gpp`:

```rust
/// GPP section IDs that represent US state/national privacy sections.
///
/// Range 7–23 per the GPP v1 specification:
/// 7=UsNat, 8=UsCa, 9=UsVa, 10=UsCo, 11=UsUt, 12=UsCt, 13=UsFl,
/// 14=UsMt, 15=UsOr, 16=UsTx, 17=UsDe, 18=UsIa, 19=UsNe, 20=UsNh,
/// 21=UsNj, 22=UsTn, 23=UsMn.
const US_SECTION_ID_RANGE: std::ops::RangeInclusive<u16> = 7..=23;

/// Extracts the `sale_opt_out` signal from the first US section in a parsed
/// GPP string.
///
/// Iterates through section IDs looking for any in the US range (7–23).
/// For the first match, decodes the section and extracts `sale_opt_out`.
///
/// Returns `Some(true)` if the user opted out of sale, `Some(false)` if they
/// did not, or `None` if no US section is present.
fn decode_us_sale_opt_out(parsed: &iab_gpp::v1::GPPString) -> Option<bool> {
    use iab_gpp::sections::us_common::OptOut;
    use iab_gpp::sections::Section;

    let us_section_id = parsed
        .section_ids()
        .find(|id| US_SECTION_ID_RANGE.contains(&(**id as u16)))?;

    match parsed.decode_section(*us_section_id) {
        Ok(section) => {
            let sale_opt_out = match &section {
                Section::UsNat(s) => match &s.core {
                    iab_gpp::sections::usnat::Core::V1(c) => &c.sale_opt_out,
                    iab_gpp::sections::usnat::Core::V2(c) => &c.sale_opt_out,
                },
                Section::UsCa(s) => &s.core.sale_opt_out,
                Section::UsVa(s) => &s.core.sale_opt_out,
                Section::UsCo(s) => &s.core.sale_opt_out,
                Section::UsUt(s) => &s.core.sale_opt_out,
                Section::UsCt(s) => &s.core.sale_opt_out,
                Section::UsFl(s) => &s.core.sale_opt_out,
                Section::UsMt(s) => &s.core.sale_opt_out,
                Section::UsOr(s) => &s.core.sale_opt_out,
                Section::UsTx(s) => &s.core.sale_opt_out,
                Section::UsDe(s) => &s.core.sale_opt_out,
                Section::UsIa(s) => &s.core.sale_opt_out,
                Section::UsNe(s) => &s.core.sale_opt_out,
                Section::UsNh(s) => &s.core.sale_opt_out,
                Section::UsNj(s) => &s.core.sale_opt_out,
                Section::UsTn(s) => &s.core.sale_opt_out,
                Section::UsMn(s) => &s.core.sale_opt_out,
                // Non-US sections — should not reach here given the ID filter.
                _ => return None,
            };
            Some(*sale_opt_out == OptOut::OptedOut)
        }
        Err(e) => {
            log::warn!("Failed to decode US GPP section {us_section_id}: {e}");
            None
        }
    }
}
```

- [ ] **Step 4: Wire it into `decode_gpp_string`**

In the same file, replace the placeholder in `decode_gpp_string`:

```rust
    let us_sale_opt_out = decode_us_sale_opt_out(&parsed);

    Ok(GppConsent {
        version: 1,
        section_ids,
        eu_tcf,
        us_sale_opt_out,
    })
```

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace -p trusted-server-core -- consent::gpp::tests`
Expected: all GPP tests pass, including the two new ones.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/consent/gpp.rs
git commit -m "Decode US sale opt-out from GPP sections"
```

---

## Task 3: Add GPP US branch to `allows_ec_creation()`

**Files:**

- Modify: `crates/trusted-server-core/src/consent/mod.rs`

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/trusted-server-core/src/consent/mod.rs`:

```rust
    #[test]
    fn ec_allowed_us_state_gpp_no_sale_opt_out() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(false),
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ec_creation(&ctx),
            "US state + GPP US sale_opt_out=false should allow EC"
        );
    }

    #[test]
    fn ec_blocked_us_state_gpp_sale_opted_out() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(true),
            }),
            ..ConsentContext::default()
        };
        assert!(
            !allows_ec_creation(&ctx),
            "US state + GPP US sale_opt_out=true should block EC"
        );
    }

    #[test]
    fn ec_blocked_us_state_gpc_overrides_gpp_us() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            gpc: true,
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(false),
            }),
            ..ConsentContext::default()
        };
        assert!(
            !allows_ec_creation(&ctx),
            "GPC should block EC even when GPP US says no opt-out"
        );
    }

    #[test]
    fn ec_us_state_tcf_takes_priority_over_gpp_us() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            tcf: Some(make_tcf_with_storage(true)),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(true),
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ec_creation(&ctx),
            "TCF consent should take priority over GPP US opt-out"
        );
    }

    #[test]
    fn ec_us_state_gpp_us_takes_priority_over_us_privacy() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(false),
            }),
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::Yes,
                opt_out_sale: PrivacyFlag::Yes,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ec_creation(&ctx),
            "GPP US should take priority over us_privacy opt-out"
        );
    }

    #[test]
    fn ec_us_state_gpp_no_us_section_falls_through_to_us_privacy() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![2],
                eu_tcf: None,
                us_sale_opt_out: None,
            }),
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::Yes,
                opt_out_sale: PrivacyFlag::No,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        assert!(
            allows_ec_creation(&ctx),
            "GPP without US section should fall through to us_privacy"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --workspace -p trusted-server-core -- consent::tests::ec_allowed_us_state_gpp`
Expected: FAIL — the GPP US branch doesn't exist yet, so `ec_allowed_us_state_gpp_no_sale_opt_out` fails (falls through to fail-closed).

- [ ] **Step 3: Add the GPP US branch to `allows_ec_creation()`**

In `crates/trusted-server-core/src/consent/mod.rs`, update `allows_ec_creation()`. The `UsState` arm currently reads:

```rust
        jurisdiction::Jurisdiction::UsState(_) => {
            if ctx.gpc {
                return false;
            }
            if let Some(tcf) = effective_tcf(ctx) {
                return tcf.has_storage_consent();
            }
            if let Some(usp) = &ctx.us_privacy {
                return usp.opt_out_sale != PrivacyFlag::Yes;
            }
            false
        }
```

Insert the GPP US check between TCF and us_privacy:

```rust
        jurisdiction::Jurisdiction::UsState(_) => {
            if ctx.gpc {
                return false;
            }
            if let Some(tcf) = effective_tcf(ctx) {
                return tcf.has_storage_consent();
            }
            // Check GPP US section for sale opt-out.
            if let Some(gpp) = &ctx.gpp {
                if let Some(opted_out) = gpp.us_sale_opt_out {
                    return !opted_out;
                }
            }
            if let Some(usp) = &ctx.us_privacy {
                return usp.opt_out_sale != PrivacyFlag::Yes;
            }
            false
        }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass, including the six new EC gating tests.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/consent/mod.rs
git commit -m "Recognize GPP US sale opt-out in EC consent gating"
```

---

## Task 4: Create Sourcepoint JS integration

**Files:**

- Create: `crates/js/lib/src/integrations/sourcepoint/index.ts`

- [ ] **Step 1: Write the test file first**

Create `crates/js/lib/test/integrations/sourcepoint/index.test.ts`:

```typescript
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { mirrorSourcepointConsent } from '../../../src/integrations/sourcepoint'

describe('integrations/sourcepoint', () => {
  beforeEach(() => {
    // Clear cookies and localStorage before each test.
    document.cookie.split(';').forEach((c) => {
      const name = c.split('=')[0].trim()
      if (name)
        document.cookie = `${name}=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/`
    })
    localStorage.clear()
  })

  afterEach(() => {
    localStorage.clear()
  })

  it('mirrors __gpp and __gpp_sid from _sp_user_consent_* localStorage', () => {
    const payload = {
      gppData: {
        gppString: 'DBABLA~BVQqAAAAAgA.QA',
        applicableSections: [7],
      },
    }
    localStorage.setItem('_sp_user_consent_36026', JSON.stringify(payload))

    const result = mirrorSourcepointConsent()

    expect(result).toBe(true)
    expect(document.cookie).toContain('__gpp=DBABLA~BVQqAAAAAgA.QA')
    expect(document.cookie).toContain('__gpp_sid=7')
  })

  it('handles multiple applicable sections', () => {
    const payload = {
      gppData: {
        gppString: 'DBABLA~BVQqAAAAAgA.QA',
        applicableSections: [7, 8],
      },
    }
    localStorage.setItem('_sp_user_consent_99999', JSON.stringify(payload))

    mirrorSourcepointConsent()

    expect(document.cookie).toContain('__gpp_sid=7,8')
  })

  it('returns false when no _sp_user_consent_* key exists', () => {
    localStorage.setItem('unrelated_key', 'value')

    const result = mirrorSourcepointConsent()

    expect(result).toBe(false)
    expect(document.cookie).not.toContain('__gpp=')
    expect(document.cookie).not.toContain('__gpp_sid=')
  })

  it('returns false for malformed JSON in localStorage', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!')

    const result = mirrorSourcepointConsent()

    expect(result).toBe(false)
    expect(document.cookie).not.toContain('__gpp=')
  })

  it('returns false when gppData is missing from payload', () => {
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify({ otherField: true })
    )

    const result = mirrorSourcepointConsent()

    expect(result).toBe(false)
    expect(document.cookie).not.toContain('__gpp=')
  })

  it('returns false when gppString is empty', () => {
    const payload = {
      gppData: {
        gppString: '',
        applicableSections: [7],
      },
    }
    localStorage.setItem('_sp_user_consent_12345', JSON.stringify(payload))

    const result = mirrorSourcepointConsent()

    expect(result).toBe(false)
    expect(document.cookie).not.toContain('__gpp=')
  })
})
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates/js/lib && npx vitest run test/integrations/sourcepoint/index.test.ts`
Expected: FAIL — module `../../../src/integrations/sourcepoint` does not exist.

- [ ] **Step 3: Implement the integration**

Create `crates/js/lib/src/integrations/sourcepoint/index.ts`:

```typescript
import { log } from '../../core/log'

const SP_CONSENT_PREFIX = '_sp_user_consent_'

interface SourcepointGppData {
  gppString: string
  applicableSections: number[]
}

interface SourcepointConsentPayload {
  gppData?: SourcepointGppData
}

function findSourcepointConsent(): SourcepointConsentPayload | null {
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i)
    if (!key?.startsWith(SP_CONSENT_PREFIX)) continue

    const raw = localStorage.getItem(key)
    if (!raw) continue

    try {
      return JSON.parse(raw) as SourcepointConsentPayload
    } catch {
      log.debug('sourcepoint: failed to parse localStorage value', { key })
      return null
    }
  }
  return null
}

function writeCookie(name: string, value: string): void {
  document.cookie = `${name}=${encodeURIComponent(value)}; path=/; SameSite=Lax`
}

/// Reads Sourcepoint consent from localStorage and mirrors it into
/// `__gpp` and `__gpp_sid` cookies for Trusted Server to read.
///
/// Returns `true` if cookies were written, `false` otherwise.
export function mirrorSourcepointConsent(): boolean {
  if (typeof localStorage === 'undefined' || typeof document === 'undefined') {
    return false
  }

  const payload = findSourcepointConsent()
  if (!payload?.gppData) {
    log.debug('sourcepoint: no GPP data found in localStorage')
    return false
  }

  const { gppString, applicableSections } = payload.gppData
  if (!gppString) {
    log.debug('sourcepoint: gppString is empty')
    return false
  }

  writeCookie('__gpp', gppString)

  if (Array.isArray(applicableSections) && applicableSections.length > 0) {
    writeCookie('__gpp_sid', applicableSections.join(','))
  }

  log.info('sourcepoint: mirrored GPP consent to cookies', {
    gppLength: gppString.length,
    sections: applicableSections,
  })

  return true
}

if (typeof window !== 'undefined') {
  mirrorSourcepointConsent()
}

export default mirrorSourcepointConsent
```

- [ ] **Step 4: Run tests**

Run: `cd crates/js/lib && npx vitest run test/integrations/sourcepoint/index.test.ts`
Expected: all 6 tests pass.

- [ ] **Step 5: Run the full JS test suite**

Run: `cd crates/js/lib && npx vitest run`
Expected: all tests pass (existing + new).

- [ ] **Step 6: Format**

Run: `cd crates/js/lib && npm run format`
Expected: no formatting issues.

- [ ] **Step 7: Commit**

```bash
git add crates/js/lib/src/integrations/sourcepoint/index.ts \
       crates/js/lib/test/integrations/sourcepoint/index.test.ts
git commit -m "Add Sourcepoint JS integration for GPP consent cookie mirroring"
```

---

## Task 5: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Build the JS bundles**

Run: `cd crates/js/lib && node build-all.mjs`
Expected: builds successfully, `dist/tsjs-sourcepoint.js` appears in the output.

- [ ] **Step 2: Full Rust build**

Run: `cargo build --workspace`
Expected: compiles with no errors.

- [ ] **Step 3: Full Rust test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 4: Clippy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Rust format check**

Run: `cargo fmt --all -- --check`
Expected: no formatting issues.

- [ ] **Step 6: Full JS test suite**

Run: `cd crates/js/lib && npx vitest run`
Expected: all tests pass.

- [ ] **Step 7: JS format check**

Run: `cd crates/js/lib && npm run format`
Expected: no formatting issues.
