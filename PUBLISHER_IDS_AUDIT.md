# Publisher-Specific IDs Audit

This document lists all publisher-specific IDs and configurations found in the codebase that are currently hardcoded to test publisher values.

## Configuration Files

### trusted-server.toml

**GAM Configuration:**
- `publisher_id = "3790"` (line 14)
- `server_url = "https://securepubads.g.doubleclick.net/gampad/ads"` (line 15)

**Equativ Configuration:**
- `sync_url = "https://adapi-srv-eu.smartadserver.com/ac?pgid=2040327&fmtid=137675&synthetic_id={{synthetic_id}}"` (line 8)
  - Page ID: `2040327`
  - Format ID: `137675`

**Test Publisher Domain:**
- `domain = "test-publisher.com"` (line 2)
- `cookie_domain = ".test-publisher.com"` (line 3)
- `origin_url = "https://origin.test-publisher.com"` (line 4)

**KV Store Names (user-specific):**
- `counter_store = "jevans_synth_id_counter"` (line 24)
- `opid_store = "jevans_synth_id_opid"` (line 25)

## Hardcoded in Source Code

### /Users/jevans/trusted-server/crates/common/src/gam.rs

**Permutive Segment Data (lines 295 and 486):**
```rust
.with_prmtvctx("129627,137412,138272,139095,139096,139218,141364,143196,143210,143211,143214,143217,144331,144409,144438,144444,144488,144543,144663,144679,144731,144824,144916,145933,146347,146348,146349,146350,146351,146370,146383,146391,146392,146393,146424,146995,147077,147740,148616,148627,148628,149007,150420,150663,150689,150690,150692,150752,150753,150755,150756,150757,150764,150770,150781,150862,154609,155106,155109,156204,164183,164573,165512,166017,166019,166484,166486,166487,166488,166492,166494,166495,166497,166511,167639,172203,172544,173548,176066,178053,178118,178120,178121,178133,180321,186069,199642,199691,202074,202075,202081,233782,238158,adv,bhgp,bhlp,bhgw,bhlq,bhlt,bhgx,bhgv,bhgu,bhhb,rts".to_string())
```

This large string contains Permutive segment IDs that appear to be captured from a specific test publisher's live traffic.

### /Users/jevans/trusted-server/crates/common/src/prebid.rs

**Equativ Integration:**
- `"pageId": 2040327` (matches config)
- `"formatId": 137675` (matches config)

### Test Files

**Test Support Files:**
- GAM publisher ID `"3790"` in test configurations
- `"test-publisher.com"` and related test domains in multiple test files

## Impact Assessment

### High Priority (Publisher-Specific)
1. **GAM Publisher ID (3790)** - Core identifier for ad serving
2. **Permutive Segments** - Large hardcoded segment string from test traffic
3. **Equativ Page/Format IDs (2040327, 137675)** - Ad network integration

### Medium Priority (Environment-Specific)
1. **Test Publisher Domains** - Should be configurable per deployment
2. **KV Store Names** - Currently user-specific (jevans_*)

### Low Priority (Infrastructure)
1. **Server URLs** - Generally standard but should be configurable

## Recommendations

1. Move hardcoded Permutive segments to configuration
2. Make GAM publisher ID environment-specific
3. Make Equativ IDs configurable per publisher
4. Generalize KV store naming convention
5. Create publisher-specific configuration templates

## Files to Update

- `trusted-server.toml` - Add permutive segments configuration
- `crates/common/src/gam.rs` - Remove hardcoded segments (lines 295, 486)
- `crates/common/src/prebid.rs` - Use configuration for Equativ IDs
- Test files - Use environment-agnostic test data