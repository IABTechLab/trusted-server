# Instream video through the auction API

Status: First take (draft v3), not yet approved. Experimental feature, off by
default.

Issue: [#361](https://github.com/IABTechLab/trusted-server/issues/361).

Related:

- [PR #1325](https://github.com/IABTechLab/trusted-server/pull/1325) is a working
  implementation against `main`. This specification uses its request contract
  and admission rules as input and restates them in the shape the rest of the
  auction subsystem uses.
- Aram's draft contract for the same issue (draft v5, 2026-10-14) supplied the
  per-provider opt-in, banner admission compatibility rule, PBS stored-impression
  handling, API framework policy, input bounds, and several field rules adopted
  here.
- The
  [config-first auction provider architecture](2026-08-10-config-first-auction-provider-architecture-design.md)
  and [APS OpenRTB](2026-07-15-aps-openrtb-first-class-integration-design.md)
  specifications both deferred video to a separate design. This is that design.

## 1. Purpose

A caller with its own video player cannot get a video ad out of Trusted Server
today. `POST /auction` accepts only banner inventory, and a video-only ad unit is
dropped before any provider sees it.

This specification adds one video use case: linear instream video, requested
directly through `POST /auction`, auctioned through `standard` and
`prebid-server` providers that the operator has enabled for video, and returned
to the caller as inline VAST. The caller's player owns playback.

The feature ships as experimental and off by default. No publisher is lined up
to adopt it. A service that does not enable video on any provider behaves as on
`main`, apart from the compatibility items in section 19.1.

## 2. Current behavior

### 2.1 Request input

`AdRequest.ad_units[].media_types` deserializes into `MediaTypes`, which has a
single `banner` field (`crates/trusted-server-core/src/auction/formats.rs`).
`convert_tsjs_to_auction_request` builds an `AdSlot` only for units with banner
sizes. A unit with only `mediaTypes.video` produces no slot, and a mixed unit is
sent as banner only. Unknown fields are ignored, so the caller receives a 200
with no bids and no error.

The canonical model already has `MediaType::Video`
(`crates/trusted-server-core/src/auction/types.rs`), but nothing constructs it
outside tests.

### 2.2 Routing

`route_auction_with_trusted_routes` passes every slot through
`eligible_banner_slot` (`crates/trusted-server-core/src/auction/routing.rs`),
which keeps only banner formats with positive dimensions that fit in `i32`.

### 2.3 Response admission

The standard and PBS response parsers never read `mtype` or `ext.prebid.type`.
Every bid is resolved through `resolve_bid_dimensions` against the slot's banner
sizes and treated as banner. APS drops any bid with `mtype` other than 1. `Bid`
has no media identity.

### 2.4 Delivery

`convert_to_openrtb_response` returns each winner's `adm` after the configured
sanitize and rewrite passes. The response has no `mtype`, `nurl`, `burl`, or
`exp`. The SSAT bid map (`build_bid_map_with_auction_id` in
`crates/trusted-server-core/src/publisher.rs`) carries `nurl`/`burl` to the GPT
render bridge, which fires them after server-side `${AUCTION_PRICE}` expansion.
The tsjs consumers of `POST /auction` (core `requestAds` and the Prebid
`trustedServer` adapter) both parse through `parseAuctionResponse`, which ignores
`nurl`/`burl` and does not read `mtype`.

`buildAdRequest` in `crates/trusted-server-js/lib/src/core/auction.ts` forwards
only `mediaTypes.banner`, so browser-originated requests never carry video.

### 2.5 Endpoint exposure

`/auction` handles only `POST`. It has no `Sec-Fetch-Site` check, no `Origin`
check, and no local `OPTIONS` handler; `OPTIONS` falls through to the publisher
origin. `/_ts/page-bids` has both gates (`publisher.rs`,
`page_bids_request_allowed`). `/auction` responses carry no `Cache-Control`.
Requests never set OpenRTB `at`, so bidders assume the default (second price
plus). These apply to banner today and are tracked as separate prerequisites in
section 20.1.

## 3. Scope and non-goals

### 3.1 In scope

- Per-provider video opt-in, validated against the profile's declared media.
- Typed linear instream video input on `POST /auction` in a video-enabled
  service.
- `imp.video` construction for video-enabled providers, including mixed
  banner/video impressions and PBS stored-impression handling.
- Media-aware response admission with inline VAST and a structural VAST check.
- A banner admission rule that keeps every bid `main` admits, apart from
  explicitly declared non-banner bids.
- Explicit `mtype` on every API winner; `nurl`/`burl` for both media types; `exp`
  for video.
- Keeping video out of the SSAT bid map and the tsjs banner render paths.
- Diagnostics for the new routing and admission outcomes.

### 3.2 Out of scope

- Browser video through Prebid.js or the tsjs `requestAds` path.
- Ad server handoff (GAM/IMA with `hb_` keys, cache or VAST URL delivery).
- Outstream, nonlinear, interstitial, and standalone (`plcmt` 3 and 4) video.
- Rewriting VAST tracking, `MediaFile`, or click URLs through first-party routes.
- Server-side `${AUCTION_PRICE}` substitution on the API path.
- Video through a mediator.
- APS video.
- Resolving VAST wrappers, fetching media, or firing notices on the server.
- In-app, CTV, and server-to-server (SSAI) callers.

Section 20 lists these as follow-ups or future-state options.

## 4. Decisions

| Decision                                    | Choice                                                                                                           | Reason                                                                                                                                                                                                                     |
| ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| First use case                              | Linear instream, `plcmt` 1 or 2, direct API only                                                                 | Smallest contract with a real consumer; browser paths need their own render design.                                                                                                                                        |
| Enablement                                  | Per-provider `media`, default `["banner"]`, checked against the profile's declared media                         | Nothing changes on upgrade. Removing `video` from a provider is the kill switch.                                                                                                                                           |
| Video-disabled service                      | Behaves as `main`: `mediaTypes.video` ignored, mixed units sent as banner, no video validation                   | An operator who has not opted in sees no new 400s.                                                                                                                                                                         |
| Protocol                                    | OpenRTB 2.6 `imp.video`, plus legacy `placement: 1` when `plcmt` is 1                                            | Both video-capable profiles speak 2.6. `placement` helps bidders still reading the 2.5 field.                                                                                                                              |
| Input validation                            | Wire type converts into a validated domain type through `TryFrom`; first failure reported by path                | A placement that exists is valid. The caller sees exactly which field failed.                                                                                                                                              |
| Bid media identity                          | `Bid.media: BidMedia` enum                                                                                       | Media is a declared fact on the bid, matched exhaustively in delivery, telemetry, and the bid map.                                                                                                                         |
| Banner admission                            | Unchanged from `main` except explicit non-banner declarations are rejected                                       | No banner bid that `main` admits is lost to video work.                                                                                                                                                                    |
| Mixed banner/video units                    | Allowed; undeclared bids on a mixed slot rejected                                                                | Lets a caller offer both. A higher-priced bid of the wrong media cannot win.                                                                                                                                               |
| VAST handling                               | Byte-for-byte passthrough after a structural check                                                               | Keeps tracking, CDATA, and macros intact while rejecting non-VAST, malformed, and multi-ad markup before winner selection.                                                                                                 |
| API frameworks                              | Input offers only OMID and SIMID; responses declaring VPAID or MRAID rejected                                    | VPAID runs executable ad code in the player. OMID is admitted even if not offered because measurement is optional; SIMID only when offered.                                                                                |
| Mediator configured                         | Configuration error when any provider enables video                                                              | `adserver_mock` is banner-only end to end. With opt-in, this blocks only deployments that asked for video.                                                                                                                 |
| `sanitize_creatives = true`                 | Allowed; VAST skips HTML processing                                                                              | Sanitization targets HTML rendered on the publisher origin; VAST is played by the caller's player. Documentation states the exclusion.                                                                                     |
| Win and billing notices                     | Returned on banner and video API winners                                                                         | One API contract for both media types. The caller fires them.                                                                                                                                                              |
| `${AUCTION_PRICE}`                          | Caller substitutes the returned `price`. Requires first-price requests (`at: 1`), a blocking prerequisite (20.1) | The macro means the clearing price. Trusted Server ranks by bid price and computes no second price, so the winning bid is the clearing price only when requests declare first price. Who substitutes does not change that. |
| Bid expiry                                  | Response `exp` is the remaining lifetime at serialization; expired winners are dropped                           | Upstream `exp` counts from the auction, not from the caller's receipt. Auction time already used must come off it.                                                                                                         |
| PBS stored impressions                      | Trusted Server sends an authoritative impression that clears every field outside this contract                   | PBS merges the request over the stored impression, so any field left out can come back from storage.                                                                                                                       |
| PBS bid adjustments and currency conversion | Unsupported for video in this phase; documented, not enforced                                                    | The returned `price` is PBS's adjusted value, so caller-substituted notices would misreport the clearing price. A settlement price is a future-state option.                                                               |
| Duplicate ad-unit codes                     | 400 when the request contains video                                                                              | Impression IDs must be unique to map video bids back. Banner-only behavior is unchanged.                                                                                                                                   |
| Production adapters                         | Fastly only in this phase                                                                                        | Video is exercised on Fastly first; other adapters compile and share the logic but are not supported for production video until open question 3 is resolved.                                                               |

## 5. Configuration

### 5.1 Provider media

```toml
[auction.providers.example-pbs]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://pbs.example.com/openrtb2/auction"
# Omitted means ["banner"]. Adding "video" opts this provider into video.
media = ["banner", "video"]

[auction]
# Upper bound on returned video `exp`, in seconds. 1 to 3600.
video_max_exp_seconds = 300
```

`ProviderConfig` in `crates/trusted-server-core/src/auction/plan.rs` gains
`media: Vec<MediaType>` with default `[Banner]`. `AuctionConfig` gains
`video_max_exp_seconds: u32` with default 300.

A service is video-enabled when at least one provider's `media` contains
`video`.

### 5.2 Profile media declaration

`OpenRtbProfileRegistration` gains a declared media list:

```rust
pub struct OpenRtbProfileRegistration {
    pub id: &'static str,
    pub default_timeout: ProfileTimeoutDefault,
    /// Media types a provider using this profile may enable.
    pub media: &'static [MediaType],
    compile: fn(&Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>>,
}
```

| Profile         | Declared `media`  |
| --------------- | ----------------- |
| `standard`      | `[Banner, Video]` |
| `prebid-server` | `[Banner, Video]` |
| `aps`           | `[Banner]`        |

A new profile registration has to choose its list; there is no default.

### 5.3 Validation

Plan compilation and `ts config validate` reject:

- a provider `media` entry not in its profile's declared media (for example,
  `video` on an `aps` provider);
- an empty or duplicate-containing `media` list;
- `native` in any `media` list;
- `video` on any provider while `[auction].mediator` is set;
- `video_max_exp_seconds` outside 1 to 3600.

Each failure names the provider ID and field.

### 5.4 Serialization compatibility

`media` serializes only when it differs from `["banner"]`, and
`video_max_exp_seconds` only when it differs from 300. A blob that does not use
video therefore still loads on an older binary. `ProviderConfig` uses
`deny_unknown_fields`, so an older binary that receives a blob with `media` fails
to load settings. Section 19.2 orders deploys around this.

## 6. Enablement scope

- In a video-enabled service, every video rule in this specification applies to
  a request in which at least one unit has `mediaTypes.video`.
- In a service that is not video-enabled, `mediaTypes.video` is ignored, mixed
  units are sent as banner only, and no video validation, bound, or duplicate-code
  rule runs. The banner admission rule in 11.1, `mtype: 1` on banner winners,
  and `nurl`/`burl` on banner winners are the only differences from `main`.

## 7. Request contract

### 7.1 Wire shape

The input follows the Prebid.js `mediaTypes.video` naming that `AdRequest`
already mirrors for banner.

```json
{
  "adUnits": [
    {
      "code": "example-preroll",
      "mediaTypes": {
        "video": {
          "context": "instream",
          "playerSize": [640, 360],
          "mimes": ["video/mp4", "application/vnd.apple.mpegurl"],
          "protocols": [2, 3, 5, 6, 7, 8, 11, 12, 13, 14],
          "minduration": 5,
          "maxduration": 30,
          "plcmt": 1,
          "startdelay": 0,
          "playbackmethod": [1],
          "api": [7],
          "skip": 1,
          "skipafter": 5
        },
        "banner": { "sizes": [[300, 250]] }
      },
      "bids": [
        { "bidder": "example-bidder", "params": { "placementId": "example" } }
      ]
    }
  ]
}
```

`banner` is optional. When both are present, the unit becomes one mixed
impression.

### 7.2 Field rules

Rows are in validation order (7.5).

| Field            | Required | Rule                                                                                                                                                                                                                            | `imp.video` target                  |
| ---------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------- |
| `context`        | Yes      | Exactly `"instream"`                                                                                                                                                                                                            | Not forwarded                       |
| `playerSize`     | Yes      | `[w, h]` or `[[w, h]]`; each 1 to 65535                                                                                                                                                                                         | `w`, `h`                            |
| `linearity`      | No       | `1`                                                                                                                                                                                                                             | `linearity`, always sent as 1       |
| `mimes`          | Yes      | 1 to 16 entries, each at most 255 bytes. A MIME essence with no parameters or wildcard, compared case-insensitively: concrete `video/*` types, `application/vnd.apple.mpegurl`, `application/x-mpegurl`, `application/dash+xml` | `mimes`, caller's spelling          |
| `protocols`      | Yes      | 1 to 16 entries, each 1 to 8 or 11 to 16 (VAST 1.0 to 4.3, inline and wrapper). DAAST 9 and 10 rejected                                                                                                                         | `protocols`                         |
| `minduration`    | Yes      | 0 to 3600                                                                                                                                                                                                                       | `minduration`                       |
| `maxduration`    | Yes      | 1 to 3600                                                                                                                                                                                                                       | `maxduration`                       |
| `plcmt`          | Yes      | `1` (instream) or `2` (accompanying content). Not inferred from `context`                                                                                                                                                       | `plcmt`; plus `placement: 1` when 1 |
| `startdelay`     | Yes      | `0` pre-roll, `1` to 86400 mid-roll offset, `-1` generic mid-roll, `-2` generic post-roll                                                                                                                                       | `startdelay`                        |
| `playbackmethod` | No       | 0 to 16 entries, each 1 to 7 (7 is continuous playback; AdCOM 8 to 11 are pause- and idle-triggered and excluded)                                                                                                               | `playbackmethod`                    |
| `api`            | No       | 0 to 16 entries, each 7 (OMID 1.0), 8 (SIMID 1.0), or 9 (SIMID 1.1). VPAID (1, 2) and MRAID/ORMMA (3 to 6) rejected                                                                                                             | `api`, always sent, `[]` when empty |
| `skip`           | No       | `0` or `1`                                                                                                                                                                                                                      | `skip`                              |
| `skipmin`        | No       | 0 to 3600                                                                                                                                                                                                                       | `skipmin`                           |
| `skipafter`      | No       | 0 to 3600                                                                                                                                                                                                                       | `skipafter`                         |

List bounds apply after deduplication; a longer list is invalid input, not
truncated.

Other fields (`pos`, `battr`, `minbitrate`, `maxbitrate`, `delivery`,
`companionad`, pod fields, `ext`) are accepted and ignored. They are never
forwarded. A later revision can add fields one at a time with their own rules.

### 7.3 Cross-field rules

Checked after every single-field rule:

- `maxduration` is at least `minduration`.
- `plcmt: 1` is invalid when `playbackmethod` contains 2 or 6 (sound-off
  autoplay). The caller classifies each break with the AdCOM Plcmt definitions;
  this rule catches only the sound-off case.
- `skipmin` and `skipafter` require `skip: 1`.

### 7.4 Request bounds

In a video request:

- at most 8 units carry `mediaTypes.video`;
- every unit `code` is 1 to 128 bytes with no control characters;
- every `code` that produces a slot is unique.

The 256 KiB body cap in `endpoints.rs` stays the outer bound.

### 7.5 Validation and errors

`mediaTypes.video` deserializes as a JSON value and converts once through
`VideoPlacement::parse(value: &serde_json::Value, path: &str)`, where `path` is
the unit's prefix (`adUnits[1].mediaTypes.video`). Type errors, missing fields,
and invalid values all come out of that conversion, so serde never produces a
fixed generic message.

The request fails with `TrustedServerError::BadRequest` reporting the first
failure in this order: units in array order; within a unit, rows of the 7.2
table in order; for each field, missing before wrong type before invalid value;
then cross-field rules; then request bounds (7.4). The message names the field
as a path and the constraint, for example
`adUnits[1].mediaTypes.video.plcmt must be 1 or 2`. It never echoes raw values
other than the unit `code`.

```rust
#[derive(Debug, derive_more::Display)]
pub enum VideoInputError {
    #[display("{path} is required")]
    Missing { path: String },
    #[display("{path} has the wrong type")]
    WrongType { path: String },
    #[display("{path} {constraint}")]
    Invalid { path: String, constraint: &'static str },
    #[display("{path} exceeds {max} entries")]
    TooManyEntries { path: String, max: usize },
    #[display("video requests allow at most {max} video units")]
    TooManyVideoUnits { max: usize },
    #[display("video requests require unique ad unit codes")]
    DuplicateCode,
}

impl core::error::Error for VideoInputError {}
```

The auction-disabled and consent-denied paths already build the auction request
before returning a no-bid, so invalid video input returns 400 there as banner
input does today.

## 8. Domain model

### 8.1 `VideoPlacement`

```rust
/// Validated player requirements for one instream video impression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "serde_json::Value")]
pub struct VideoPlacement {
    // `TryFrom<serde_json::Value>` calls `parse` with the path `video`.
    mimes: Vec<String>,
    protocols: Vec<i32>,
    min_duration: i32,
    max_duration: i32,
    plcmt: VideoPlcmt,
    start_delay: i32,
    api: Vec<i32>,
    playback_methods: Vec<i32>,
    skip: Option<VideoSkip>,
}

/// OpenRTB 2.6 placement types accepted for instream video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoPlcmt {
    Instream,
    AccompanyingContent,
}

/// Skippability offered by the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoSkip {
    NotSkippable,
    Skippable { min: Option<i32>, after: Option<i32> },
}
```

Fields are private, with read accessors. `parse` is the only constructor, and
the serde `try_from` path calls it, so no code path holds an unvalidated
placement and routing does not revalidate.

IAB code lists stay `Vec<i32>` because the generated OpenRTB `Video` type
(`crates/trusted-server-openrtb/src/generated.rs`) uses `i32`. `plcmt` and `skip`
get enums because request building matches on them and the cross-field rules
depend on them.

Player dimensions live in the slot's
`AdFormat { media_type: MediaType::Video, .. }` entry, so format-based
telemetry keeps working. The banner dimension index must not see that entry
(section 11).

### 8.2 `AdSlot.video`

```rust
pub struct AdSlot {
    pub id: String,
    pub formats: Vec<AdFormat>,
    /// Player requirements when `formats` contains a video entry.
    pub video: Option<VideoPlacement>,
    // existing fields unchanged
}
```

Invariant: `video` is `Some` exactly when `formats` has one `MediaType::Video`
entry. `convert_tsjs_to_auction_request` is the only producer and maintains it.
Routing treats a slot that breaks the invariant as banner-only and logs at
`warn`.

### 8.3 `Bid.media`

```rust
/// Media identity and media-specific facts for an admitted bid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BidMedia {
    Banner,
    Video(VideoBid),
}

/// Video response facts retained for the caller's player.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoBid {
    pub apis: Vec<i32>,
    pub protocol: Option<i32>,
    pub duration: Option<i32>,
    pub creative_width: Option<u32>,
    pub creative_height: Option<u32>,
    pub exp: Option<i32>,
}
```

`Bid` gets `pub media: BidMedia` after `creative`. The field has no
`#[serde(default)]`, following the rule on `MediaType` in `types.rs`: a missing
media type must fail, not become banner. No production path deserializes `Bid`;
it is serialized only into debug output.

Every provider parser sets `media` explicitly. APS and `adserver_mock` always set
`BidMedia::Banner`.

## 9. Routing

### 9.1 Per-provider eligibility

The compiled provider plan carries the provider's `media` list.
`eligible_banner_slot` becomes `eligible_slot(slot, media: &[MediaType])`:

1. If `media` contains `Banner`, keep banner formats with positive `i32`
   dimensions, as today.
2. If `media` contains `Video` and the slot has a placement and exactly one video
   format, keep that format and the placement.
3. If nothing is kept, the slot is not sent to that provider.

The match on `MediaType` is exhaustive. A provider without video receives the
banner half of a mixed slot and skips a video-only slot. A provider with
`media = ["video"]` receives the video half of a mixed slot and skips a
banner-only slot, so it can never receive or win banner inventory. A video slot
is never sent as banner. Each provider's bid dimension index is built from the
slots that provider received.

### 9.2 Omission diagnostics

`RoutingDiagnostics` gains `media_omitted_slot_count`, a saturating count of
(provider, slot) pairs dropped because the provider's `media` did not cover the
slot. It is logged at `info` with the existing `unroutable_bidder_count` line
and exposed through `routing_metadata`.

A provider left with no slots goes through the existing
`skipped_no_eligible_provider_ids` path.

## 10. Provider request

### 10.1 `imp.video`

| `imp.video` field              | Source                                               |
| ------------------------------ | ---------------------------------------------------- |
| `mimes`                        | Placement `mimes`                                    |
| `protocols`                    | Placement `protocols`                                |
| `minduration`, `maxduration`   | Placement durations                                  |
| `plcmt`                        | 1 or 2 from `VideoPlcmt`                             |
| `placement`                    | `1` when `plcmt` is 1; omitted otherwise             |
| `linearity`                    | Always 1                                             |
| `startdelay`                   | Placement start delay                                |
| `w`, `h`                       | Video `AdFormat` dimensions through `to_openrtb_i32` |
| `api`                          | Placement `api`; `[]` when empty on PBS (10.2)       |
| `playbackmethod`               | Placement playback methods; `[]` when empty on PBS   |
| `skip`, `skipmin`, `skipafter` | From `VideoSkip`, when present                       |

For `standard` providers, fields the placement does not set are omitted, as the
generated types serialize them today. PBS providers get the authoritative form
in 10.2.

`imp.banner` is built from banner formats only, exactly as today. A video-only
slot has no `imp.banner`. A banner-only request produces byte-identical provider
bodies before and after this change (section 18).

Consent, `regs`, `user`, `device`, EIDs, `tmax`, `tagid`, `secure`, signing, and
profile extensions use the existing shared path. Signing binds request ID,
host, scheme, and timestamp; it does not cover `imp` content. Video adds no new
privacy inputs.

### 10.2 PBS stored impressions

Slots with no inline bidder params keep the stored-request fallback
(`imp.ext.prebid.storedrequest.id`). PBS merges the incoming impression over the
stored one, and any field the incoming impression omits survives from storage.
Clearing `banner` and `video.api` alone is not enough: a stored `placement: 1`
would survive under an incoming `plcmt: 2`, and stored `playbackmethod`, skip
fields, `battr`, or pod fields would reappear. The merged request would no longer
match the validated placement.

Every video impression sent to a PBS provider is therefore authoritative:

- `imp.video` lists every field of the generated OpenRTB 2.6 `Video` type. Fields
  this contract sets carry their values. Every other field is written as `null`
  (scalars and objects) or `[]` (arrays), including `placement` when `plcmt` is
  2, `playbackmethod` and `api` when empty, and absent skip fields. `video.ext`
  is `null`.
- `imp.audio` and `imp.native` are `null`. `imp.banner` is `null` on a video-only
  impression.
- Other impression-level fields (`imp.ext`, `tagid`, `bidfloor`) are left as
  they are, so stored bidder configuration and floors still apply.

PBS treats `null` as removal and replaces arrays whole when it merges. The
generated types skip `None` and empty `Vec` fields on serialization, so the PBS
profile writes the authoritative form as a `serde_json::Value` step after
serialization. A unit test asserts the complete `imp.video` key set against the
generated type, so a codegen update that adds a field fails the test until the
field is handled.

Stored impressions used by video units should hold only bidder configuration
(`imp.ext`). This is documented for operators; the clearing above is the
enforcement. Section 18.5 checks the merged bidder request against a real PBS.
Trusted Server does not request PBS caching; the PBS account must return markup
in `adm`.

### 10.3 PBS bid adjustments and currency conversion

With bid adjustments or currency conversion enabled, PBS returns an adjusted USD
`price`, and the bidder's own bid is in `ext.origbidcpm` and `ext.origbidcur`.
The caller substitutes `${AUCTION_PRICE}` with the returned `price` (section 13),
so on such an account the notices would report the adjusted value instead of the
bidder's clearing price.

PBS accounts that adjust or convert video bids are unsupported in this phase.
The configuration guide and the Prebid integration guide state this. Trusted
Server does not detect or reject these bids. A settlement price returned
alongside `price` is a future-state option (20.3).

## 11. Response admission

Standard and PBS parsers replace their `resolve_bid_dimensions` call with a
shared `resolve_bid_media(value, slot_id, index) -> Result<AdmittedMedia,
BidRejectionReason>`. Admission runs completely before winner selection, so every
admitted bid is deliverable as-is.

`SlotBidDimensions::from_formats` indexes every format it is given today. Once a
slot can carry a video format, it must index banner formats only. Otherwise a
640x360 player beside a 300x250 banner would make a dimensionless banner bid
ambiguous, and a banner bid declaring 640x360 would be admitted at the player
size. The slot's video dimensions and placement are stored in a separate field
of `SlotBidDimensions` and used only by video admission. A mixed slot with no
banner formats left after routing has an empty banner index, so banner bids on
it reject as `media_type_not_offered`.

Media type is read from `mtype` or PBS `ext.prebid.type`. It is a bidder
declaration, not a trust boundary.

### 11.1 Banner-only slots

`main`'s admission rules apply unchanged, with one exception: a well-formed
explicit non-banner declaration (`mtype` integer 2, 3, or 4, or
`ext.prebid.type` equal to `"video"`, `"native"`, or `"audio"`) rejects as
`unsupported_media_type`. Every other value, including absent, `null`, `1`,
`"banner"`, non-integers, and unknown strings, is ignored as it is today.

Every bid `main` admits on a banner-only slot is admitted, except bids the
exception rejects. This rule applies in every service, video-enabled or not.

### 11.2 Slots with video

`null` counts as absent.

| Declaration                                                     | Video-only slot                 | Mixed slot                      |
| --------------------------------------------------------------- | ------------------------------- | ------------------------------- |
| `mtype` and `ext.prebid.type` both present and different        | Reject `media_type_conflict`    | Reject `media_type_conflict`    |
| Non-integer `mtype`, unknown `ext.prebid.type`, or native/audio | Reject `unsupported_media_type` | Reject `unsupported_media_type` |
| Banner                                                          | Reject `media_type_not_offered` | Banner admission (main rules)   |
| Video                                                           | Video admission (11.3)          | Video admission (11.3)          |
| None                                                            | Video admission (11.3)          | Reject `ambiguous_media_type`   |

Inference applies only when the slot offered exactly one media type, so a
higher-priced bid of the wrong media cannot win a mixed slot.

### 11.3 Video bid checks

In order:

1. `adm` is present and non-empty. Otherwise reject `video_adm_missing`. This
   excludes cache-only and `nurl`-served VAST.
2. `adm` is at most `MAX_CREATIVE_SIZE` (1 MiB, shared with `creative.rs`; the
   constant becomes `pub(crate)`). Otherwise reject `video_adm_too_large`.
3. `adm` passes the VAST check in 11.4.
4. `w`/`h`, when present and nonzero, are positive `i32` values. They describe
   the creative and are not checked against the player size. Missing or zero
   means unknown. Known values are kept as `creative_width`/`creative_height`;
   internal `Bid.width`/`height` fall back to the player size.
5. `protocol`, when present, is in the placement's `protocols`. Otherwise reject
   `video_protocol_mismatch`.
6. `dur`, when present, is positive and within `[minduration, maxduration]`.
   Otherwise reject `video_duration_out_of_range`.
7. Declared APIs (`apis`, plus legacy `api` normalized into the same list):
   values 1 to 6 reject; 7 (OMID) is admitted even if not offered; 8 and 9
   (SIMID) are admitted only if offered. Otherwise reject `video_api_mismatch`.
   Admitted values are deduplicated into `VideoBid::apis`.
8. `exp`, when present and positive, is kept as the upstream value. The
   remaining lifetime is computed at delivery (12.2).

Wrong JSON types or out-of-range integers in these fields reject as
`invalid_bid`. A rejected bid never displaces a valid sibling.

### 11.4 VAST check

The check uses `quick-xml`'s streaming `Reader` with a reused buffer. It is a new
workspace dependency: pure Rust, builds for `wasm32-wasip1` and
`wasm32-unknown-unknown`, and does not expand entities. No existing dependency
parses XML.

Reaching EOF without a reader error is not enough. The reader can reach EOF with
an unclosed element, and it parses attributes lazily, so a malformed attribute is
reported only when the attributes are iterated. The check therefore tracks state
itself. The markup passes when:

- before the root, only an optional BOM, whitespace, one XML declaration,
  comments, and processing instructions appear;
- the root element is `VAST` and has a `version` attribute;
- there is no `DOCTYPE` declaration (which also excludes `ENTITY` declarations);
- every event reads without a reader error, with end-name checking on;
- the attributes of every start and empty element are iterated with checks on,
  and each attribute value unescapes without error;
- every text node unescapes without error, which rejects undefined entity
  references (only the five predefined entities and character references are
  allowed);
- an element depth counter returns to zero exactly once, at the root's end tag,
  and is zero at EOF;
- after the root, only whitespace, comments, and processing instructions appear;
- the root has exactly one `Ad` child element (depth 1).

CDATA sections are not unescaped and need no reference check. Unescaped values
are discarded after the check, and the admitted `adm` is the original string.

Failures of the `Ad` rule reject as `video_adm_ad_count`. Every other failure
rejects as `video_adm_not_vast`.

The check does not validate VAST schema, media URLs, or wrapper targets, and it
is not evidence of playability. The admitted `adm` is the original string, not a
re-serialization.

Cost is modeled, not measured. A streaming read that builds no tree should be
linear in input size. Section 18.5 requires measuring it under Viceroy before
this design is approved.

### 11.5 Rejection reasons

`BidRejectionReason` gains:

| Variant                   | `as_str`                      | Trigger     |
| ------------------------- | ----------------------------- | ----------- |
| `UnsupportedMediaType`    | `unsupported_media_type`      | 11.1, 11.2  |
| `MediaTypeConflict`       | `media_type_conflict`         | 11.2        |
| `MediaTypeNotOffered`     | `media_type_not_offered`      | 11.2        |
| `AmbiguousMediaType`      | `ambiguous_media_type`        | 11.2        |
| `VideoAdmMissing`         | `video_adm_missing`           | 11.3 step 1 |
| `VideoAdmTooLarge`        | `video_adm_too_large`         | 11.3 step 2 |
| `VideoAdmNotVast`         | `video_adm_not_vast`          | 11.4        |
| `VideoAdmAdCount`         | `video_adm_ad_count`          | 11.4        |
| `VideoProtocolMismatch`   | `video_protocol_mismatch`     | 11.3 step 5 |
| `VideoDurationOutOfRange` | `video_duration_out_of_range` | 11.3 step 6 |
| `VideoApiMismatch`        | `video_api_mismatch`          | 11.3 step 7 |

They flow through `ResponseAdmissionDiagnostics` into each provider's
`response_admission` metadata like the existing reasons.

### 11.6 Provider body caps

Whole provider responses stay capped at 1 MiB for `standard` and 2 MiB for PBS
(`MAX_PLANNED_RESPONSE_BYTES` in `auction/provider.rs` and
`UPSTREAM_RTB_MAX_RESPONSE_BYTES` in `integrations/mod.rs`).
An over-cap body fails the whole provider, banner bids included, so a near-cap
VAST costs that provider's entire response. This phase keeps the caps.

## 12. API response

### 12.1 Bid fields

| Field                                                    | Banner winner                                  | Video winner                                         |
| -------------------------------------------------------- | ---------------------------------------------- | ---------------------------------------------------- |
| `adm`                                                    | Existing sanitize/rewrite result               | Admitted VAST, unchanged                             |
| `mtype`                                                  | `1` (new)                                      | `2`                                                  |
| `w`, `h`                                                 | Existing                                       | Known creative size only; omitted when unknown       |
| `nurl`, `burl`                                           | Provider value after notification policy (new) | Same                                                 |
| `exp`                                                    | Omitted                                        | Always present: remaining lifetime in seconds (12.2) |
| `apis`                                                   | Omitted                                        | Admitted APIs, omitted when empty                    |
| `protocol`, `dur`                                        | Omitted                                        | Provider values, when present                        |
| `id`, `impid`, `price`, `adomain`, `crid`, `adid`, `ext` | Existing                                       | Existing                                             |

New banner fields are additive. Callers that ignore unknown fields see no change.

### 12.2 Video expiry

OpenRTB `exp` is the time the bidder will wait between the auction and the
impression. Time spent waiting for other providers and building the response
has already used part of it, so the response does not return the upstream value
unchanged.

At serialization, for each video winner:

1. The effective lifetime is the upstream `exp` when positive, otherwise
   `video_max_exp_seconds`, and never more than `video_max_exp_seconds`.
2. Elapsed time is `OrchestrationResult::total_time_ms` rounded up to whole
   seconds. The orchestrator measures it from auction start, which is no later
   than any provider dispatch, so the result errs short.
3. The returned `exp` is effective lifetime minus elapsed seconds.
4. When that is less than 1, the winner is dropped with delivery reason
   `video_bid_expired` and the slot returns no bid.

The returned `exp` therefore counts from the moment the caller receives the
response. With auction timeouts in the low seconds and lifetimes of minutes,
step 4 is expected to be rare; it exists so that a short upstream `exp` is never
extended.

### 12.3 Notifications

`nurl` and `burl` are returned after `apply_notification_policy`, so
`suppress_all` and `suppress_seats` still remove them. Trusted Server does not
fire API-path notices for either media type and does not expand
`${AUCTION_PRICE}` in them or in VAST.

`${AUCTION_PRICE}` is the clearing price. Trusted Server ranks by bid price and
computes no second price, so the returned `price` is the clearing price only
when the request told bidders the auction is first price. This contract
therefore depends on every provider request carrying `at: 1` (section 20.1).
Without it, bidders are told the auction is second price while notices report
their own bid. The same gap exists today on the SSAT bid-map path, which expands
the macro with the bid price.

`parseAuctionResponse` keeps ignoring `nurl`/`burl` in this phase. Neither tsjs
consumer fires them, so there is no overlap with the GPT bridge, which reads its
URLs from the server-injected bid map.

### 12.4 Creative processing

Video `adm` never enters `sanitize_creative_html`, the creative rewriter, or
TSJS injection, regardless of `sanitize_creatives` and `rewrite_creatives`.
HTML processing would change XML structure, CDATA sections, and tracking URLs.
The VAST check in 11.4 is the only server-side inspection.

## 13. Caller responsibilities

The API reference and a player integration guide (section 20.2) document these.
They are requirements on the caller, not behavior Trusted Server enforces.

- Disable VPAID in the player or ad SDK before initialization (for IMA,
  `VpaidMode.DISABLED`). Declared APIs cannot exclude VPAID, because a VAST can
  carry an undeclared VPAID media file or a wrapper can resolve to one.
- Never render `adm` as HTML.
- Synchronize consent before each request: wait for the CMP to report its state
  and for that state to reach the consent cookies Trusted Server reads. If
  consent is withdrawn before the impression, cancel playback and fire nothing.
  Fill VAST privacy macros from the current CMP state.
- Play at most one linear creative, from the first `Ad`, after wrapper
  resolution.
- Treat `nurl` as a win notice, not a VAST tag URL. Fire `burl` at the VAST
  Impression event. Resolve `${AUCTION_PRICE}` in both, and in VAST, at the
  returned `price`, which is the clearing price under first-price requests
  (12.3).
- Do not start playback after `exp` elapses, counted from response receipt. The
  returned value is already reduced by auction time (12.2).
- Do not retry a `4xx`. A `5xx` or timeout may be retried once as a new request.

## 14. Browser and SSAT paths

- Only `POST /auction` creates video slots.
- `build_bid_map_with_auction_id` skips winners with `BidMedia::Video`. The GPT
  bridge renders HTML into banner slots and has no player.
- `parseAuctionResponse` skips bids whose `mtype` is present and not 1. Bids
  without `mtype` keep the current behavior.
- `buildAdRequest` keeps forwarding only `mediaTypes.banner`.
- Creative-opportunity formats with `media_type = "video"` produce no video slot.
  Rejecting them at config load is a follow-up.

## 15. Security and privacy

- VAST is third-party markup returned unchanged. Tracking, media, and wrapper
  URLs stay on third-party hosts, and the browser contacts them directly through
  every wrapper hop. The first-party property Trusted Server provides for banner
  creatives through rewriting does not extend to video in this phase.
  `proxy.allowed_domains` and `rewrite.exclude` do not apply to VAST.
- VPAID and MRAID are excluded at input and at admission (7.2, 11.3). The player
  must still disable VPAID (section 13).
- The VAST check rejects `DOCTYPE`, which removes entity-expansion payloads
  before they reach a player's XML parser.
- Trusted Server makes no new outbound requests for video: no wrapper
  resolution, no media fetches, no notice firing.
- Video uses the same consent, GPP, US privacy, EC, EID, and signing path as
  banner. `regs.coppa` is never set, so operators must not enable video on
  child-directed inventory.
- `/auction` accepts cross-site `POST` today (2.5). Video responses carry
  `nurl`/`burl`, so a page or bot that calls `/auction` and fires `burl` without
  playing creates invalid traffic. The API reference documents this exposure.
  The local `OPTIONS` and origin checks in section 20.1 close the cross-site
  part.
- Input lists, unit counts, codes, and VAST size are bounded (7.2, 7.4, 11.3).
- VAST, `nurl`, and `burl` are never logged above `trace` and never written to
  telemetry. Rejections log at `debug` with auction ID, provider, impression ID,
  reason, and byte lengths.
- With PBS debug enabled, PBS `ext.debug` (including bidder payloads and VAST)
  is copied into `provider_details[].metadata` and returned. Operators must not
  enable PBS debug in production.

## 16. Telemetry and diagnostics

- `bid_row` sets `media_type` from `Bid.media` for every bid, not from the slot's
  first format. A video bid in a mixed slot reports `video`.
- Video bid rows report the player size in `slot_w`/`slot_h`.
- New rejection reasons appear in existing admission diagnostics in the
  response `ext`.
- `media_omitted_slot_count` appears in routing diagnostics and response
  metadata.

No `AuctionEventRow` schema change is needed; `media_type` already exists.
Per-slot, per-rejection, and winner-drop event kinds, and media-specific
Tinybird rollups, are follow-ups (section 20.2).

## 17. Required code areas

| File                                                           | Change                                                                                              |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/auction/plan.rs`               | `ProviderConfig.media`, compile-time validation (5.3)                                               |
| `crates/trusted-server-core/src/auction_config_types.rs`       | `video_max_exp_seconds`                                                                             |
| `crates/trusted-server-core/src/auction/profile.rs`            | `media` on `OpenRtbProfileRegistration`                                                             |
| `crates/trusted-server-core/src/auction/formats.rs`            | `MediaTypes.video`, conversion and errors, bounds, response fields                                  |
| `crates/trusted-server-core/src/auction/types.rs`              | `VideoPlacement`, `VideoPlcmt`, `VideoSkip`, `AdSlot.video`, `BidMedia`, `VideoBid`, `Bid.media`    |
| `crates/trusted-server-core/src/auction/routing.rs`            | `eligible_slot`, omission diagnostics                                                               |
| `crates/trusted-server-core/src/auction/openrtb.rs`            | `imp.video`, `resolve_bid_media`, VAST check, rejection reasons                                     |
| `crates/trusted-server-core/src/integrations/prebid.rs`        | PBS parser uses `resolve_bid_media`; `"banner": null` on stored-fallback video imps                 |
| `crates/trusted-server-core/src/integrations/aps.rs`           | Set `BidMedia::Banner`                                                                              |
| `crates/trusted-server-core/src/integrations/adserver_mock.rs` | Set `BidMedia::Banner`                                                                              |
| `crates/trusted-server-core/src/auction/telemetry.rs`          | `media_type` and slot size from `Bid.media`                                                         |
| `crates/trusted-server-core/src/publisher.rs`                  | Bid map skips video                                                                                 |
| `crates/trusted-server-core/src/creative.rs`                   | `MAX_CREATIVE_SIZE` becomes `pub(crate)`                                                            |
| `crates/trusted-server-cli`                                    | `ts config validate` covers 5.3                                                                     |
| `crates/trusted-server-js/lib/src/core/auction.ts`             | `parseAuctionResponse` skips non-banner `mtype`                                                     |
| `Cargo.toml`, `crates/trusted-server-core/Cargo.toml`          | Add `quick-xml`                                                                                     |
| `docs/guide/`                                                  | See 19.3                                                                                            |
| `CHANGELOG.md`                                                 | Video under Added (experimental); banner `mtype`/`nurl`/`burl` and the 11.1 exception under Changed |

Adding `Bid.media` and `AdSlot.video` touches every struct literal in tests. New
fields go in declaration order, not first.

## 18. Acceptance criteria

### 18.1 Configuration and enablement

- `media` defaults to `["banner"]` and round-trips without serializing the
  default; `video_max_exp_seconds` likewise.
- Validation rejects `video` on `aps`, video with a mediator, `native`, empty or
  duplicate lists, and an out-of-range `video_max_exp_seconds`, at load and in
  `ts config validate`.
- Every profile registration declares `media`.
- In a service without video, a video request behaves as `main`: mixed units go
  out as banner, video-only units produce no slot, and no video rule or bound
  runs.

### 18.2 Request and validation

- A video-only unit produces one slot with a video format and a placement; a
  mixed unit produces one slot with both.
- `playerSize` `[640, 360]` and `[[640, 360]]` are equivalent.
- Each row of 7.2 and each rule in 7.3 and 7.4 has an at-limit and over-limit or
  invalid test, including playback method 7 accepted, API 1 to 6 rejected, the
  `placement` rule, the skip fields, a 129-byte code, and a ninth video unit.
- The 400 message follows the 7.5 order for missing, wrong-type, invalid,
  cross-field, and request-level failures.
- Unknown video fields are accepted and not forwarded.
- Duplicate codes with video return 400; without video, behavior is unchanged.
- The same invalid video request yields the same 400 message on Axum,
  Cloudflare, and Spin (parity suite).

### 18.3 Routing and request building

- Standard and PBS providers with video receive `imp.video` with every mapped
  field, including `api: []` when empty and `placement: 1` when `plcmt` is 1.
- A provider without video receives the banner half of a mixed slot and skips a
  video-only slot; `media_omitted_slot_count` increments.
- A provider with `media = ["video"]` receives the video half of a mixed slot,
  skips a banner-only slot, and cannot win banner inventory.
- A PBS video impression lists every generated `Video` field, with `null` or `[]`
  for unset ones (including `placement` under `plcmt: 2`), and carries
  `"audio": null`, `"native": null`, and, when video-only, `"banner": null`. The
  key-set test fails when the generated type gains a field.
- Every provider request carries `at: 1` once the prerequisite in 20.1 lands.
- A banner-only request produces byte-identical provider bodies before and after
  the change, for all three profiles, apart from `at`.

### 18.4 Response admission and delivery

- Banner-only slots admit `mtype: null`, `"garbage"`, `1.0`, and unknown
  `ext.prebid.type` strings exactly as `main`, and reject explicit 2 to 4.
- Each row of the 11.2 table has a test, for standard and PBS.
- On a mixed slot with a 300x250 banner and a 640x360 player, a dimensionless
  banner bid resolves to 300x250, and a banner bid declaring 640x360 rejects.
  The same holds when the banner side offers several sizes.
- HTML labeled `mtype: 2`, malformed XML, a non-`VAST` root, a missing `version`,
  a `DOCTYPE`, multiple roots, zero and two `Ad` children, empty `adm`, and `adm`
  over 1 MiB each reject with the 11.5 reason.
- A truncated document with the root left open, a malformed attribute (missing
  quote, duplicate name), an undefined entity reference in text and in an
  attribute value, and non-whitespace text before and after the root each reject
  as `video_adm_not_vast`.
- VAST with a BOM, leading whitespace, an XML declaration, comments, CDATA, and
  `${AUCTION_PRICE}` is admitted and returned byte for byte, with
  `sanitize_creatives` and `rewrite_creatives` both enabled.
- Unoffered OMID is admitted; unoffered SIMID and any of API 1 to 6 reject.
- Protocol and duration mismatches reject with their reasons.
- In a mixed slot, a higher-priced bid of the wrong or undeclared media cannot
  win.
- A video winner returns `mtype: 2`, `protocol`, `dur`, `apis`, `nurl`, and
  `burl`, and no `w`/`h` when unknown.
- Returned `exp` equals the effective lifetime minus elapsed auction seconds,
  rounded up: for an upstream `exp` of 300 and a 1,200 ms auction it is 298; with
  no upstream `exp` it starts from `video_max_exp_seconds`; an upstream value
  above the cap is capped. A winner whose remaining lifetime is under 1 second is
  dropped as `video_bid_expired`.
- A banner winner returns `mtype: 1`, `nurl`, and `burl`.
- Notification suppression removes `nurl`/`burl` for both media types.
- The SSAT bid map excludes video winners; `parseAuctionResponse` drops `mtype` 2
  and keeps bids without `mtype`.
- A near-cap provider response fails that provider and its banner bids.
- An end-to-end `handle_auction` test with stubbed standard and PBS transports
  selects the highest compatible video bid.
- No log line above `trace` and no telemetry row contains VAST or notice URLs.

### 18.5 Validation beyond unit tests

- VAST check timing under Viceroy for a 10 KiB and a 1 MiB fixture, recorded in
  the implementation PR.
- A repeatable run against a real Prebid Server with stored auction responses
  shows `imp.video` accepted and VAST in `adm`. The resolved bidder request is
  inspected for a video impression whose stored impression carries `banner`, a
  VPAID `api`, `placement: 1`, `playbackmethod`, and skip fields, under an
  incoming `plcmt: 2` with no playback methods or skip fields. None of the stored
  media fields reach the bidder. The run covers each PBS implementation the
  deployment uses.
- A reference player (Video.js with IMA, VPAID disabled) plays an InLine and a
  Wrapper response from `POST /auction`, and outbound requests for `nurl`,
  `burl`, and VAST trackers are counted.

### 18.6 CI gates

All gates in `CLAUDE.md` pass, including the wasm clippy targets with
`quick-xml` added.

## 19. Compatibility, rollout, and rollback

### 19.1 Changes for every service

- Banner API winners gain `mtype: 1`, `nurl`, and `burl`.
- On banner-only slots, bids that explicitly declare non-banner media reject
  (11.1).

Everything else in this specification applies only to a video-enabled service.
The first-price prerequisite (20.1) also changes every service, under its own
approval.

### 19.2 Deploy and rollback

1. Deploy the binary with no config change. Video is off everywhere. Confirm
   banner `provider_health` and `seat_yield` are unchanged for 24 hours.
2. Enable video on one provider in an internal staging service, push the
   config, and verify with a real `POST /auction` video request. `GET /health`
   stays healthy through a settings failure, so it is not sufficient.
3. A production service opts in one provider at a time.

To stop video, remove `video` from the provider's `media` and push. This stops
new video routing. In-flight auctions finish, and VAST already returned stays
playable until its `exp`. With the reusable sandbox, an instance keeps the
previous config until it retires, so the worst case until no new video ad can
start is config propagation plus the sandbox lifetime plus
`video_max_exp_seconds`.

To roll back the binary, first remove `media` and `video_max_exp_seconds` from
the config and push, then roll back. An older binary that receives those fields
fails to load settings (5.4).

### 19.3 Documentation

- `docs/guide/api-reference.md`, `POST /auction`: video request and response,
  status codes, caller responsibilities (section 13), and the invalid-traffic
  exposure (section 15). Labeled experimental.
- `docs/guide/auction-orchestration.md`: an "Instream video" section under
  "Input and Output Formats", and `AdSlot`/`Bid` updates under "Data
  Structures".
- `docs/guide/configuration.md`: provider `media`, `video_max_exp_seconds`,
  validation rules, deploy and rollback order.
- `docs/guide/creative-processing.md`: VAST skips sanitize and rewrite. The
  `CHANGELOG.md` line stating that `sanitize_creatives` strips executable markup
  gains a sentence excluding VAST.
- `docs/guide/integrations/prebid.md`: PBS stored-impression rules (10.2), adjusted
  or converted accounts unsupported for video (10.3), no PBS
  caching, PBS debug off in production.

## 20. Related work

### 20.1 Prerequisites

These change `/auction` for every caller. Each gets its own issue and approval.

Blocking: this specification does not ship before it.

- First-price auctions: every provider request sets `at: 1`. Decided; an issue
  is to be filed and linked from #361. The notice contract
  in 12.3 is correct only under it. It also corrects the SSAT bid-map path,
  which already expands `${AUCTION_PRICE}` with the bid price.

Tracked separately, not blocking:

- Local `OPTIONS /auction` answered with `403`, plus `Sec-Fetch-Site` and
  `Origin` checks matching `/_ts/page-bids`.
- `Cache-Control: private, no-store` on every `/auction` response.

### 20.2 Follow-ups

Each gets an issue linked from #361.

- Player integration guide with an IMA example, expanding section 13.
- Telemetry event kinds (`slot`, `bid_rejection`, `winner_drop`) and
  media-specific Tinybird rollups, once a service runs video in production.
- Win attribution by selected-candidate identity (provider, response ordinal,
  bid ordinal) instead of a search by bid ID. This predates video.
- Total auction deadline enforcement, before the experimental label comes off.
- Prebid shim behavior for video-only units with server-owned bidders.
- Rejecting `media_type = "video"` creative-opportunity formats at config load.
- Stripping PBS debug metadata from `/auction` responses.
- Raising provider body caps for video-enabled plans after peak-memory
  measurement.
- A COPPA operator flag.
- Unique ad-unit codes for banner-only requests.

### 20.3 Future-state options

- Ad server handoff: PBS cache pass-through or a first-party VAST URL with `hb_`
  keys for GAM/IMA, the dominant web instream flow.
- Prebid.js video through the `trustedServer` adapter (`mtype: 2` to `vastXml`).
- First-party VAST delivery: rewritten tracking and media URLs reusing the relay
  from the
  [browser-blocked domain rewriting](2026-10-01-browser-blocked-domain-rewriting-design.md)
  design, and server-fired notices.
- A settlement price returned alongside `price` (PBS `origbidcpm`/`origbidcur`),
  and server-side macro substitution with it.
- Video mediation through `adserver_mock`.
- Outstream and other `plcmt` values, with a renderer design.
- APS video, once APS confirms its contract.
- In-app, CTV, and SSAI callers.

## 21. Open questions

1. Should video be withheld when `ec_allowed()` is false? Aram's draft says yes.
   This draft routes video under the same consent rules as banner.
2. Does the measured VAST check cost (18.5) fit the Compute CPU budget at the
   1 MiB cap, or should the video cap be lower than the banner cap?
3. What deadline enforcement do Cloudflare and Spin apply to provider calls, and
   what is needed to support production video there?
4. Should the experimental label come off on fixed adoption criteria (provider
   acceptance of `imp.video`, delivered-to-impressed conversion, time to first
   frame, eCPM against the publisher's previous path)?
   Resolved:

- Playback methods 1 to 7 are accepted. AdCOM 7 is continuous playback; 8 to 11
  are pause- and idle-triggered methods and are excluded.
- First-price requests (`at: 1`) are a blocking prerequisite (20.1), chosen over
  a settlement-price model.
- PBS accounts with bid adjustments or currency conversion are unsupported for
  video in this phase (10.3).

## 22. Alternatives considered

| Alternative                                                                               | Why not chosen                                                                                                                       |
| ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Video capability on the profile only, no provider opt-in                                  | Every standard and PBS provider would receive `imp.video` on upgrade, with no kill switch short of a redeploy.                       |
| Video as "every profile except APS"                                                       | New profiles would receive video without declaring it.                                                                               |
| Strip video when a mediator is configured                                                 | Silently ignores an operator's opt-in. With opt-in, a config error is clearer and blocks no banner-only deployment.                  |
| 400 every video request when a mediator is configured                                     | Reports a deployment fact as a per-request client error and loses banner demand on mixed requests.                                   |
| Reject mixed banner/video units                                                           | Smaller, but forces callers to split one placement into two units with different codes.                                              |
| Config error for video with `sanitize_creatives = true`                                   | Blocks video for every service that sanitizes banner, though VAST is never rendered as HTML on the publisher origin.                 |
| Keep the wire type as the domain type with `is_valid() -> bool`                           | Lets invalid placements exist in `AdSlot`, forces revalidation in routing, and gives one generic error message.                      |
| Newtype every IAB code list                                                               | Every enum converts back to `i32` at the OpenRTB boundary, doubling the type surface for range checks the constructor already does.  |
| `Bid.video: Option<VideoBid>`                                                             | Media identity implied by `is_some()`.                                                                                               |
| VAST byte scan with no parser                                                             | Avoids a dependency but admits malformed XML.                                                                                        |
| VAST passthrough with no check                                                            | Admits arbitrary strings as video and lets them win.                                                                                 |
| Server-side macro substitution in this phase                                              | Needs first-price auctions and a settlement-price model first.                                                                       |
| `nurl`/`burl` on video only                                                               | Gives the API two notice contracts that differ by media type.                                                                        |
| Caller substitutes `${AUCTION_PRICE}` with the bid price while requests stay second price | The macro means the clearing price; reporting the bid price under a second-price declaration misstates it no matter who substitutes. |
| Clear only `banner` and `video.api` on PBS stored fallback                                | Every other omitted field (`placement`, `playbackmethod`, skip fields, pod fields) survives the PBS merge.                           |
| Disallow PBS stored fallback for video slots                                              | Removes the stored-request flow many PBS deployments depend on; the authoritative impression covers the merge risk instead.          |
| Return upstream `exp` unchanged                                                           | Extends the bidder's validity window by the auction time already spent.                                                              |
| Prohibit `media = ["video"]`                                                              | A video-only demand endpoint is a valid configuration; gating banner retention on `Banner` costs one condition.                      |
| Bundle the `/auction` origin, cache, and first-price fixes here                           | They change behavior for every service and need their own approvals.                                                                 |

## Review log

- Round 1 (Codex, separate session): checked the current-behavior claims against
  the source and found them accurate. Found six issues. Caller substitution of
  `${AUCTION_PRICE}` with the bid price is wrong while requests still declare
  second price. The PBS stored-impression merge can bring back fields other
  than `banner` and `video.api`, and the generated `Video` type drops empty
  arrays. Routing would still send banners to a `media = ["video"]` provider.
  The banner dimension index would include the video player size on mixed
  slots. Reaching EOF in `quick-xml` does not prove the XML is well formed.
  Returning upstream `exp` unchanged extends the bidder's validity window.
  Fixed all six: first-price requests became a blocking prerequisite (20.1),
  PBS video impressions became authoritative (10.2), banner retention is gated
  on `Banner` (9.1), the banner index holds banner formats only (11), the VAST
  check tracks depth, attributes, references, and content outside the root
  (11.4), and `exp` is returned as remaining lifetime (12.2). Also closed the
  playback method question.
- Next: round 1 found blocking issues, so round 2 is required, in a fresh
  session, and review continues until a round finds nothing blocking.
