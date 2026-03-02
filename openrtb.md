Below is a **complete (non-verbatim) OpenRTB 2.6 schema cheat-sheet** in Markdown: **all objects + field names + types + required/default notes**, plus a few **JSON examples**. It’s meant to be copy/pasteable while you build a library.

> Conventions used below
>
> * `object` = JSON object, `object[]` = array of objects
> * “required / recommended / optional” follows the spec language
> * “default X” means if omitted, interpret as X
> * Many integer enums are defined in **AdCOM 1.0** lists (Device Types, APIs, etc.).
> * `ext` exists on most objects as an `object` for exchange/bidder extensions.

---

## 1) Bid Request

### 1.1 `BidRequest` (top-level)

| field     | type     |        req? | default | notes                                                                   |
| --------- | -------- | ----------: | ------: | ----------------------------------------------------------------------- |
| `id`      | string   |    required |         | request/auction ID                                                      |
| `imp`     | `Imp[]`  |    required |         | 1+ impressions                                                          |
| `site`    | `Site`   | recommended |         | **mutually exclusive** with `app`                                       |
| `app`     | `App`    | recommended |         | **mutually exclusive** with `site`                                      |
| `device`  | `Device` | recommended |         | device info                                                             |
| `user`    | `User`   | recommended |         | user/audience info                                                      |
| `test`    | integer  |    optional |       0 | 1=test mode                                                             |
| `at`      | integer  |    optional |       2 | auction type (1=FP, 2=SP+)                                              |
| `tmax`    | integer  |    optional |         | max time (ms) for bids                                                  |
| `wseat`   | string[] |    optional |         | allowlist seats (use wseat **or** bseat)                                |
| `bseat`   | string[] |    optional |         | blocklist seats; “at most only one of wseat and bseat”                  |
| `allimps` | integer  |    optional |       0 | 1=all imps in context (roadblocking)                                    |
| `cur`     | string[] |    optional |         | allowed currencies (ISO-4217)                                           |
| `wlang`   | string[] |    optional |         | allowlist creative languages (ISO-639-1); only one of `wlang`/`wlangb`  |
| `wlangb`  | string[] |    optional |         | allowlist creative languages (BCP-47); only one of `wlang`/`wlangb`     |
| `bcat`    | string[] |    optional |         | blocked categories (taxonomy per `cattax`)                              |
| `cattax`  | integer  |    optional |       1 | category taxonomy id                                                    |
| `badv`    | string[] |    optional |         | blocked advertiser domains                                              |
| `bapp`    | string[] |    optional |         | blocked app store IDs                                                   |
| `source`  | `Source` |    optional |         | supply source + decisioning entity                                      |
| `regs`    | `Regs`   |    optional |         | regulations in force                                                    |
| `ext`     | object   |    optional |         | extensions                                                              |

---

### 1.2 `Source`

| field    | type          |     req? | default | notes                          |
| -------- | ------------- | -------: | ------: | ------------------------------ |
| `fd`     | integer       | optional |       0 | “final decision” on sale (0/1) |
| `tid`    | string        | optional |         | transaction ID                 |
| `pchain` | string        | optional |         | payment chain                  |
| `schain` | `SupplyChain` | optional |         | supply chain object            |
| `ext`    | object        | optional |         |                                |

*(Schema listed here; field meanings above are per spec section 3.2.2, referenced by BidRequest.)*

---

### 1.3 `Regs`

| field        | type    |     req? | default | notes             |
| ------------ | ------- | -------: | ------: | ----------------- |
| `coppa`      | integer | optional |         | COPPA flag        |
| `gdpr`       | integer | optional |         | GDPR flag         |
| `us_privacy` | string  | optional |         | US Privacy string |
| `ext`        | object  | optional |         |                   |

---

### 1.4 `Imp`

| field               | type       |     req? | default | notes                                   |
| ------------------- | ---------- | -------: | ------: | --------------------------------------- |
| `id`                | string     | required |         | imp ID                                  |
| `metric`            | `Metric[]` | optional |         | metrics array                           |
| `banner`            | `Banner`   | optional |         | banner opportunity                      |
| `video`             | `Video`    | optional |         | video opportunity                       |
| `audio`             | `Audio`    | optional |         | audio opportunity                       |
| `native`            | `Native`   | optional |         | native opportunity                      |
| `pmp`               | `Pmp`      | optional |         | PMP container                           |
| `displaymanager`    | string     | optional |         | mediation/SDK/player name               |
| `displaymanagerver` | string     | optional |         | version                                 |
| `instl`             | integer    | optional |       0 | interstitial/fullscreen                 |
| `tagid`             | string     | optional |         | placement/ad tag id                     |
| `bidfloor`          | float      | optional |       0 | CPM floor                               |
| `bidfloorcur`       | string     | optional |   "USD" | ISO-4217                                |
| `clickbrowser`      | integer    | optional |         | in-app click browser type               |
| `secure`            | integer    | optional |         | 1=HTTPS required                        |
| `iframebuster`      | string[]   | optional |         | supported iframe busters                |
| `rwdd`              | integer    | optional |       0 | reward viewing                          |
| `ssai`              | integer    | optional |       0 | server-side ad insertion mode           |
| `exp`               | integer    | optional |         | seconds between auction and impression  |
| `ext`               | object     | optional |         |                                         |

---

### 1.5 `Metric`

| field    | type   |        req? | default | notes                        |
| -------- | ------ | ----------: | ------: | ---------------------------- |
| `type`   | string |    required |         | metric name                  |
| `value`  | float  |    required |         | probability metrics in 0..1  |
| `vendor` | string | recommended |         | e.g. “EXCHANGE”              |
| `ext`    | object |    optional |         |                              |

---

### 1.6 `Banner`

| field      | type       |        req? | default | notes                        |
| ---------- | ---------- | ----------: | ------: | ---------------------------- |
| `format`   | `Format[]` | recommended |         | allowed sizes/flex sizes     |
| `w`        | integer    | recommended |         | width if no `format`         |
| `h`        | integer    | recommended |         | height if no `format`        |
| `btype`    | integer[]  |    optional |         | blocked banner types         |
| `battr`    | integer[]  |    optional |         | blocked creative attributes  |
| `pos`      | integer    |    optional |         | placement position           |
| `mimes`    | string[]   |    optional |         | supported MIME types         |
| `topframe` | integer    |    optional |         | 1=in top frame               |
| `expdir`   | integer[]  |    optional |         | expandable directions        |
| `api`      | integer[]  |    optional |         | supported APIs               |
| `id`       | string     |    optional |         | companion banner id          |
| `vcm`      | integer    |    optional |         | companion render mode        |
| `ext`      | object     |    optional |         |                              |

---

### 1.7 `Video`

Core fields (most commonly implemented):

| field            | type       |       req? | default | notes                                               |
| ---------------- | ---------- | ---------: | ------: | --------------------------------------------------- |
| `mimes`          | string[]   |   required |         | supported video MIME types (see examples)           |
| `minduration`    | integer    |   optional |         | min seconds (mutually exclusive with `rqddurs`)     |
| `maxduration`    | integer    |   optional |         | max seconds (mutually exclusive with `rqddurs`)     |
| `rqddurs`        | integer[]  |   optional |         | exact acceptable durations; exclusive with min/max  |
| `protocols`      | integer[]  |   optional |         | supported protocols (AdCOM)                         |
| `w`              | integer    |   optional |         | player width (DIPS in examples)                     |
| `h`              | integer    |   optional |         | player height                                       |
| `startdelay`     | integer    |   optional |         | start delay                                         |
| `placement`      | integer    |   optional |         | placement subtype                                   |
| `linearity`      | integer    |   optional |         | linear/nonlinear                                    |
| `skip`           | integer    |   optional |         | 1=skippable                                         |
| `skipmin`        | integer    |   optional |       0 | duration threshold for skippable                    |
| `skipafter`      | integer    |   optional |       0 | seconds before skip enabled                         |
| `battr`          | integer[]  |   optional |         | blocked creative attrs                              |
| `maxextended`    | integer    |   optional |         | max extension seconds, 0/blank=no extension         |
| `minbitrate`     | integer    |   optional |         | kbps                                                |
| `maxbitrate`     | integer    |   optional |         | kbps                                                |
| `boxingallowed`  | integer    |   optional |         | letterboxing allowed (example shows 1)              |
| `playbackmethod` | integer[]  |   optional |         | playback methods (example shows array)              |
| `delivery`       | integer[]  |   optional |         | delivery methods (example shows array)              |
| `pos`            | integer    |   optional |         | placement position (ATF, etc.)                      |
| `companionad`    | `Banner[]` |   optional |         | companion ads in VAST sense                         |
| `companiontype`  | integer[]  |   optional |         | companion types (example)                           |
| `podid`          | string     |   optional |         | pod identifier (pod bidding)                        |
| `podseq`         | integer    |   optional |       0 | sequence of pod in stream                           |
| `slotinpod`      | integer    |   optional |       0 | seller-guaranteed slot position                     |
| `maxseq`         | integer    |   optional |         | max ads in pod (examples)                           |
| `poddur`         | integer    |   optional |         | pod duration seconds (examples)                     |
| `mincpmpersec`   | float      |   optional |         | min CPM/sec for dynamic pod portion                 |
| `sequence`       | integer    | deprecated |       0 | deprecated sequencing                               |
| `ext`            | object     |   optional |         |                                                     |

---

### 1.8 `Audio`

Same shape as Video for most fields (mimes/durations/protocols/startdelay/placement/linearity/skip/bitrates/delivery/companion* and pod-bidding fields).
Additionally supports `podid`, `podseq`, `slotinpod`, `maxseq`, `poddur` like Video (see changelog note). 

*(If you want, I can output Audio as a full table mirroring Video 1:1; it’s long but straightforward.)*

---

### 1.9 `Native`

| field     | type      |        req? | default | notes                                           |
| --------- | --------- | ----------: | ------: | ----------------------------------------------- |
| `request` | string    |    required |         | JSON-encoded request payload (Native 1.0/1.1+)  |
| `ver`     | string    | recommended |         | version of Dynamic Native Ads API               |
| `api`     | integer[] |    optional |         | supported APIs                                  |
| `battr`   | integer[] |    optional |         | blocked creative attrs                          |
| `ext`     | object    |    optional |         |                                                 |

---

### 1.10 `Format`

| field    | type    |     req? | notes               |
| -------- | ------- | -------: | ------------------- |
| `w`      | integer | optional | width DIPS          |
| `h`      | integer | optional | height DIPS         |
| `wratio` | integer | optional | flex ratio width    |
| `hratio` | integer | optional | flex ratio height   |
| `wmin`   | integer | optional | min width for flex  |
| `ext`    | object  | optional |                     |

---

### 1.11 `Pmp`

| field             | type     |     req? | default | notes                 |
| ----------------- | -------- | -------: | ------: | --------------------- |
| `private_auction` | integer  | optional |       0 | 1=only deals allowed  |
| `deals`           | `Deal[]` | optional |         | deals array           |
| `ext`             | object   | optional |         |                       |

### 1.12 `Deal`

| field         | type     |     req? | default | notes                                                    |
| ------------- | -------- | -------: | ------: | -------------------------------------------------------- |
| `id`          | string   | required |         | deal id                                                  |
| `bidfloor`    | float    | optional |       0 | CPM floor                                                |
| `bidfloorcur` | string   | optional |   "USD" | ISO-4217                                                 |
| `at`          | integer  | optional |         | auction override; includes “3 = bidfloor is deal price”  |
| `wseat`       | string[] | optional |         | allowlist buyer seats                                    |
| `wadomain`    | string[] | optional |         | allowlist advertiser domains                             |
| `ext`         | object   | optional |         |                                                          |

---

### 1.13 `Site`

| field           | type        |        req? | notes                          |
| --------------- | ----------- | ----------: | ------------------------------ |
| `id`            | string      | recommended | exchange site id               |
| `name`          | string      |    optional |                                |
| `domain`        | string      |    optional |                                |
| `cattax`        | integer     |    optional | taxonomy                       |
| `cat`           | string[]    |    optional | site categories                |
| `sectioncat`    | string[]    |    optional | section categories             |
| `pagecat`       | string[]    |    optional | page/view categories           |
| `page`          | string      |    optional | page URL                       |
| `ref`           | string      |    optional | referrer URL                   |
| `search`        | string      |    optional | search string                  |
| `mobile`        | integer     |    optional | 0/1 mobile optimized           |
| `privacypolicy` | integer     |    optional | 0/1                            |
| `publisher`     | `Publisher` |    optional |                                |
| `content`       | `Content`   |    optional |                                |
| `keywords`      | string      |    optional | CSV; exclusive with `kwarray`  |
| `kwarray`       | string[]    |    optional | exclusive with `keywords`      |
| `ext`           | object      |    optional |                                |

---

### 1.14 `App`

| field           | type        |        req? | default | notes                            |
| --------------- | ----------- | ----------: | ------: | -------------------------------- |
| `id`            | string      | recommended |         | exchange app id                  |
| `name`          | string      |    optional |         |                                  |
| `bundle`        | string      |    optional |         | store id / package / numeric id  |
| `domain`        | string      |    optional |         |                                  |
| `storeurl`      | string      |    optional |         |                                  |
| `cattax`        | integer     |    optional |       1 |                                  |
| `cat`           | string[]    |    optional |         |                                  |
| `sectioncat`    | string[]    |    optional |         |                                  |
| `pagecat`       | string[]    |    optional |         |                                  |
| `ver`           | string      |    optional |         |                                  |
| `privacypolicy` | integer     |    optional |         |                                  |
| `paid`          | integer     |    optional |         | 0 free / 1 paid                  |
| `publisher`     | `Publisher` |    optional |         |                                  |
| `content`       | `Content`   |    optional |         |                                  |
| `keywords`      | string      |    optional |         | exclusive w/ `kwarray`           |
| `kwarray`       | string[]    |    optional |         | exclusive w/ `keywords`          |
| `ext`           | object      |    optional |         |                                  |

---

### 1.15 `Publisher`

| field    | type     |     req? | notes                |
| -------- | -------- | -------: | -------------------- |
| `id`     | string   | optional | publisher id         |
| `name`   | string   | optional |                      |
| `cattax` | integer  | optional | taxonomy             |
| `cat`    | string[] | optional | publisher categories |
| `domain` | string   | optional |                      |
| `ext`    | object   | optional |                      |

---

### 1.16 `Content`

A large object; here’s the full field list (types) in one place (all optional unless noted):

* IDs & naming: `id`(string), `episode`(integer), `title`(string), `series`(string), `season`(string)
* Media metadata: `artist`(string), `genre`(string), `album`(string), `url`(string)
* Keywords & categories: `keywords`(string), `kwarray`(string[]), `cattax`(integer), `cat`(string[])
* Quality/ratings: `prodq`(integer), `rating`(string), `userrating`(string), `qagmediarating`(integer)
* Context: `context`(integer), `livestream`(integer), `sourcerel`(integer), `len`(integer), `language`(string), `langb`(string), `embeddable`(integer)
* Entity subobjects: `producer`(`Producer`), `network`(`Network`), `channel`(`Channel`)
* `ext`(object)

*(This list corresponds to the spec’s Content section 3.2.16 which also introduces Network/Channel in 2.6; see object definitions below.)*

---

### 1.17 `Producer`

| field    | type     |     req? | default | notes |
| -------- | -------- | -------: | ------: | ----- |
| `id`     | string   | optional |         |       |
| `name`   | string   | optional |         |       |
| `cattax` | integer  | optional |       1 |       |
| `cat`    | string[] | optional |         |       |
| `domain` | string   | optional |         |       |
| `ext`    | object   | optional |         |       |

---

### 1.18 `Device`

| field            | type        |        req? | default | notes                                      |
| ---------------- | ----------- | ----------: | ------: | ------------------------------------------ |
| `geo`            | `Geo`       | recommended |         |                                            |
| `dnt`            | integer     | recommended |         | do-not-track                               |
| `lmt`            | integer     | recommended |         | limit-ad-tracking                          |
| `ua`             | string      |    optional |         | raw UA string; see `sua` guidance          |
| `sua`            | `UserAgent` |    optional |         | structured UA-CH data                      |
| `ip`             | string      |    optional |         | IPv4                                       |
| `ipv6`           | string      |    optional |         | IPv6                                       |
| `devicetype`     | integer     |    optional |         | AdCOM device type                          |
| `make`           | string      |    optional |         |                                            |
| `model`          | string      |    optional |         |                                            |
| `os`             | string      |    optional |         |                                            |
| `osv`            | string      |    optional |         |                                            |
| `hwv`            | string      |    optional |         |                                            |
| `h`              | integer     |    optional |         | screen px height                           |
| `w`              | integer     |    optional |         | screen px width                            |
| `ppi`            | integer     |    optional |         |                                            |
| `pxratio`        | float       |    optional |         |                                            |
| `js`             | integer     |    optional |         |                                            |
| `geofetch`       | integer     |    optional |         | geo API availability                       |
| `flashver`       | string      |    optional |         |                                            |
| `language`       | string      |    optional |         | ISO-639-1; only one of `language`/`langb`  |
| `langb`          | string      |    optional |         | BCP-47; only one of `language`/`langb`     |
| `carrier`        | string      |    optional |         |                                            |
| `mccmnc`         | string      |    optional |         | MCC-MNC with dash                          |
| `connectiontype` | integer     |    optional |         | AdCOM connection type                      |
| `ifa`            | string      |    optional |         | ID for advertiser use                      |
| `didsha1`        | string      |  deprecated |         |                                            |
| `didmd5`         | string      |  deprecated |         |                                            |
| `dpidsha1`       | string      |  deprecated |         |                                            |
| `dpidmd5`        | string      |  deprecated |         |                                            |
| `macsha1`        | string      |  deprecated |         |                                            |
| `macmd5`         | string      |  deprecated |         |                                            |
| `ext`            | object      |    optional |         |                                            |

---

### 1.19 `Geo`

| field       | type    |     req? | notes                 |
| ----------- | ------- | -------: | --------------------- |
| `lat`       | float   | optional | latitude              |
| `lon`       | float   | optional | longitude             |
| `type`      | integer | optional | location source type  |
| `accuracy`  | integer | optional | accuracy (meters)     |
| `lastfix`   | integer | optional | time since last fix   |
| `ipservice` | integer | optional | IP geo service        |
| `country`   | string  | optional | country code          |
| `region`    | string  | optional | region code           |
| `metro`     | string  | optional | metro code            |
| `city`      | string  | optional | city                  |
| `zip`       | string  | optional | zip/postal            |
| `utcoffset` | integer | optional | minutes offset        |
| `ext`       | object  | optional |                       |

---

### 1.20 `User`

| field        | type     |       req? | notes                                        |
| ------------ | -------- | ---------: | -------------------------------------------- |
| `id`         | string   |   optional | exchange user id                             |
| `buyeruid`   | string   |   optional | buyer-specific id                            |
| `yob`        | integer  | deprecated | year of birth (deprecated in 2.6 changelog)  |
| `gender`     | string   | deprecated | deprecated in 2.6 changelog                  |
| `keywords`   | string   |   optional | CSV; exclusive with kwarray                  |
| `kwarray`    | string[] |   optional | exclusive with keywords                      |
| `customdata` | string   |   optional | buyer data                                   |
| `geo`        | `Geo`    |   optional | user home base (vs device current)           |
| `data`       | `Data[]` |   optional | data segments                                |
| `consent`    | string   |   optional | consent string                               |
| `eids`       | `EID[]`  |   optional | external IDs                                 |
| `ext`        | object   |   optional |                                              |

---

### 1.21 `Data`

| field     | type        |     req? | notes              |
| --------- | ----------- | -------: | ------------------ |
| `id`      | string      | optional | data provider id   |
| `name`    | string      | optional | data provider name |
| `segment` | `Segment[]` | optional | segments           |
| `ext`     | object      | optional |                    |

### 1.22 `Segment`

| field   | type   |     req? | notes         |
| ------- | ------ | -------: | ------------- |
| `id`    | string | optional | segment id    |
| `name`  | string | optional | segment name  |
| `value` | string | optional | segment value |
| `ext`   | object | optional |               |

---

### 1.23 `Network` (added in 2.6)

| field    | type   |     req? | notes        |
| -------- | ------ | -------: | ------------ |
| `id`     | string | optional | network id   |
| `name`   | string | optional | network name |
| `domain` | string | optional | domain       |
| `ext`    | object | optional |              |

### 1.24 `Channel` (added in 2.6)

| field    | type   |     req? | notes        |
| -------- | ------ | -------: | ------------ |
| `id`     | string | optional | channel id   |
| `name`   | string | optional | channel name |
| `domain` | string | optional | domain       |
| `ext`    | object | optional |              |

---

### 1.25 `SupplyChain` (SChain)

| field      | type                |     req? | notes            |
| ---------- | ------------------- | -------: | ---------------- |
| `complete` | integer             | required | 0/1 completeness |
| `nodes`    | `SupplyChainNode[]` | required | 1+ nodes         |
| `ver`      | string              | required | schain version   |
| `ext`      | object              | optional |                  |

### 1.26 `SupplyChainNode`

| field    | type    |     req? | notes                             |
| -------- | ------- | -------: | --------------------------------- |
| `asi`    | string  | required | ad system identifier              |
| `sid`    | string  | required | seller ID within `asi`            |
| `hp`     | integer | required | 1 if payment handled by this node |
| `rid`    | string  | optional | request id                        |
| `name`   | string  | optional | business name                     |
| `domain` | string  | optional | business domain                   |
| `ext`    | object  | optional |                                   |

---

### 1.27 `EID`

| field    | type    |     req? | notes                  |
| -------- | ------- | -------: | ---------------------- |
| `source` | string  | required | identity source domain |
| `uids`   | `UID[]` | required | 1+ uids                |
| `ext`    | object  | optional |                        |

### 1.28 `UID`

| field   | type    |     req? | notes           |
| ------- | ------- | -------: | --------------- |
| `id`    | string  | required | the identifier  |
| `atype` | integer | optional | ID type (AdCOM) |
| `ext`   | object  | optional |                 |

---

### 1.29 `UserAgent` (UA-CH structured)

| field          | type             |     req? | notes                         |
| -------------- | ---------------- | -------: | ----------------------------- |
| `browsers`     | `BrandVersion[]` | optional | browser brand/version list    |
| `platform`     | `BrandVersion`   | optional | platform brand/version        |
| `mobile`       | integer          | optional | 0/1/… (UA-CH Mobile guidance) |
| `architecture` | string           | optional | e.g. “x86”, “arm”             |
| `bitness`      | string           | optional | e.g. “64”                     |
| `model`        | string           | optional | device model                  |
| `source`       | integer          | optional | 0 default; UA source (AdCOM)  |
| `ext`          | object           | optional |                               |

### 1.30 `BrandVersion`

| field     | type     |     req? | notes                     |
| --------- | -------- | -------: | ------------------------- |
| `brand`   | string   | required | brand identifier          |
| `version` | string[] | optional | version components array  |
| `ext`     | object   | optional |                           |

---

## 2) Bid Response

### 2.1 `BidResponse` (top-level)

| field        | type        |      req? | default | notes                            |
| ------------ | ----------- | --------: | ------: | -------------------------------- |
| `id`         | string      |  required |         | matches request id               |
| `seatbid`    | `SeatBid[]` | optional* |         | 1+ if bidding; empty for no-bid  |
| `bidid`      | string      |  optional |         | bidder response id               |
| `cur`        | string      |  optional |   "USD" | ISO-4217                         |
| `customdata` | string      |  optional |         | cookie data, base85-safe         |
| `nbr`        | integer     |  optional |         | no-bid reason code reference     |
| `ext`        | object      |  optional |         |                                  |

*In practice: for no-bid you can return HTTP 204 or `{"id":"...","seatbid":[]}` (example patterns shown in spec). 

---

### 2.2 `SeatBid`

| field   | type    |     req? | default | notes                  |
| ------- | ------- | -------: | ------: | ---------------------- |
| `bid`   | `Bid[]` | required |         | 1+ bids                |
| `seat`  | string  | optional |         | buyer seat ID          |
| `group` | integer | optional |       0 | 1=must win as a group  |
| `ext`   | object  | optional |         |                        |

---

### 2.3 `Bid`

Core transaction fields:

| field   | type   |     req? | notes                |
| ------- | ------ | -------: | -------------------- |
| `id`    | string | required | bid id               |
| `impid` | string | required | references `Imp.id`  |
| `price` | float  | required | CPM price            |
| `nurl`  | string | optional | win notice URL       |
| `burl`  | string | optional | billing notice URL   |
| `lurl`  | string | optional | loss notice URL      |
| `adm`   | string | optional | ad markup inline     |

Creative / policy / rendering metadata (common fields):

| field            | type      |       req? | notes                                 |
| ---------------- | --------- | ---------: | ------------------------------------- |
| `adid`           | string    |   optional | preloaded ad id                       |
| `adomain`        | string[]  |   optional | advertiser domains                    |
| `bundle`         | string    |   optional | app store id                          |
| `iurl`           | string    |   optional | representative image URL              |
| `cid`            | string    |   optional | campaign id                           |
| `crid`           | string    |   optional | creative id                           |
| `tactic`         | string    |   optional | tactic id                             |
| `cattax`         | integer   |   optional | default 1 taxonomy                    |
| `cat`            | string[]  |   optional | creative categories                   |
| `attr`           | integer[] |   optional | creative attributes                   |
| `apis`           | integer[] |   optional | supported APIs                        |
| `api`            | integer   | deprecated | deprecated in favor of `apis`         |
| `protocol`       | integer   |   optional | video response protocol subtype       |
| `qagmediarating` | integer   |   optional | media rating                          |
| `language`       | string    |   optional | ISO-639-1; exclusive w/ `langb`       |
| `langb`          | string    |   optional | BCP-47; exclusive w/ `language`       |
| `dealid`         | string    |   optional | references `Deal.id`                  |
| `w`              | integer   |   optional | width DIPS                            |
| `h`              | integer   |   optional | height DIPS                           |
| `wratio`         | integer   |   optional | flex width ratio                      |
| `hratio`         | integer   |   optional | flex height ratio                     |
| `exp`            | integer   |   optional | seconds bidder will wait              |
| `dur`            | integer   |   optional | duration seconds (video/audio)        |
| `mtype`          | integer   |   optional | 1 banner, 2 video, 3 audio, 4 native  |
| `slotinpod`      | integer   |   optional | slot eligibility/position in pod      |
| `ext`            | object    |   optional | extensions                            |

---

## 3) Minimal end-to-end JSON examples

### 3.1 Banner request (skeleton)

```json
{
  "id": "req-1",
  "at": 2,
  "tmax": 120,
  "imp": [
    {
      "id": "1",
      "banner": {
        "format": [{"w": 300, "h": 250}],
        "pos": 1
      },
      "bidfloor": 0.10,
      "bidfloorcur": "USD"
    }
  ],
  "site": { "page": "https://example.com" },
  "device": { "ua": "Mozilla/5.0", "ip": "203.0.113.1", "js": 1 }
}
```

### 3.2 Bid response (skeleton)

```json
{
  "id": "req-1",
  "cur": "USD",
  "seatbid": [
    {
      "seat": "buyer-1",
      "bid": [
        {
          "id": "bid-1",
          "impid": "1",
          "price": 1.23,
          "adm": "<html>...</html>",
          "adomain": ["advertiser.com"],
          "crid": "creative-123"
        }
      ]
    }
  ]
}
```

### 3.3 Pod-bidding style example (from the spec samples)

The PDF includes full request/response examples for pod bidding; see the excerpted sample structure around `video.podid`, `slotinpod`, `maxseq`, `poddur` etc. 


