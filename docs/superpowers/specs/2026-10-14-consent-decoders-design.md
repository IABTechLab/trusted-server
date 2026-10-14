# Design Spec: The consent decoders live in the crates named for their standards

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1335, one of the removals listed in IABTechLab/trusted-server#777
**Last updated:** 2026-10-14

> **Why this spec exists.** Core decoded the TCF, GPP and US Privacy strings itself, in `crates/trusted-server-core/src/consent`, and the permission signal crates named for those standards read what core had decoded. A standard is a vendor's thing as much as a product is, so the decoding belongs in the crate for the standard, and core keeps only the interface every signal module fills.

## 1. Problem

The consent module in core extracted the consent strings from cookies and headers, decoded each of them, settled a standalone TC String against the EU section of a GPP string, expired a record by age, constructed a US Privacy string from a Global Privacy Control header, and built a request context whose fields were those standards' fields. The permission signal crates for TCF, GPP and US Privacy existed and were selected by name, yet the decoding they were named for was core's. The auction endpoints, the OpenRTB request builder, the mock ad server, the demand crates and the Cloudflare adapter read core's decoded fields directly, and the `[consent]` settings table was typed in core with those standards' keys.

The permission model made how a permission becomes held the business of modules outside core. Decoding the strings those modules are named for inside core left the model half done, left core carrying every change the standards make, and meant a scheme core had never heard of could not be read the way the shipped schemes were.

## 2. Design

**Each decoder lives in the crate named for its standard.** The TC String decoder, with the record type and the purpose mapping, is in `crates/permission-signal/tcf`. The GPP string decoder, with the `iab_gpp` dependency, is in `crates/permission-signal/gpp`. The US Privacy string decoder is in `crates/permission-signal/us-privacy`. The Global Privacy Control header is read in `crates/permission-signal/gpc`. Each crate reads its own string from the request's cookies and headers, which is the evidence every module already reads.

**A module reads its signal once, and reports a record.** The permission signal trait gains `read`, called by core once per request before any permission is asked, which answers a `SignalRecord`. A record carries the scheme's name, the string as received, whether it decoded, could not be read, has expired or was left as received, where it came from (the request, or the deployment's declared defaults), whether the module says the regulation its scheme answers for applies, the identifiers of the sections a sectioned signal carries, and the decoded form, which is the module's own type and is opaque to core. The module reads its record back in `signal`, `withdraws` and `valid_signal`, so every reader of the request sees the same record and the module answers the same as the bid request builder and the identity graph about expiry and which string is in force.

**Core keeps the neutral types and nothing of any standard.** `ConsentContext` is the records the configured modules reported, in module order, and the request's jurisdiction. `ConsentSource` says where a signal came from. `SignalStatus` says what became of it. Core reads a record by scheme name only where it forwards the strings as received, which is the OpenRTB request builder still in core, and that reader moves out of core with OpenRTB.

**The settings stay where they were.** `[consent]` keeps its keys. Core reads `mode`, which says whether the modules decode or leave the strings as received, and holds every other key as written for the modules to read into types of their own. The TCF module reads `check_expiration`, `max_consent_age_days` and `conflict_resolution`, and the US Privacy module reads `us_privacy_defaults`. A key no linked module reads is not refused, where the typed table refused it, because core no longer knows which keys exist.

**The policy names schemes by the modules' names.** `signals.<scheme>.authoritative` in `permissions.yaml` says whether a record of that scheme answers, for any scheme, where only `signals.tcf` was parsed. `signals.us_opt_out.sources` lists the opt-out sources by the names the modules give them, where it listed three fixed names. The shipped sample keeps the same text.

**Readers read the resolved state, or ask the crate for the standard.** The server-side auction gate reads the resolved permission state, running the auction where `advertising_marketing.first_party.contextual` is set, where it read the decoded TCF record for Purpose 1 under a jurisdiction rule of its own. The EID gate already read the permission state. The identity graph record stores every signal a module vouched for, keyed by scheme, in the same JSON shape as before for the two schemes it stored. Telemetry reads whether any module says its regulation applies. Where a demand implementation needs a string in its standard's own form, it asks the crate for that standard, which answers from the record: `tcf::tc_string`, `tcf::applies`, `gpp::gpp_string`, `gpp::section_ids`, `us_privacy::us_privacy_string` and `gpc::signaled`. The TCF crate asks the GPP crate for the EU section of a GPP string and for its section identifiers, and the US Privacy crate asks the GPC crate whether the header was sent, as one vendor crate may depend on another.

## 3. What is not in this change

- The OpenRTB request builder leaving core, which another series does. Until then the builder in core reads the strings by scheme name through the neutral interface, and the OpenRTB crate asks the crates for the standards once it is out.
- Any change to the strings a bid request carries, the cookies the modules read, or the names a deployment writes in `[permission-signal] modules`.
- The Google Additional Consent string, which core never populated. The field that carried it, and the demand policy flag that sent it, are removed rather than kept as an interface nothing fills.

## 4. Tests

- Core's consent pipeline asks each module in order, reports the records they answer, offers them the jurisdiction, and reads nothing with no modules.
- Each crate reads its string from the request's cookies and headers, decodes it, records an unreadable string as such, leaves it as received in proxy mode, and vouches only for a string that decoded. The TCF crate settles a standalone string against the EU section of a GPP string under each conflict rule, expires a stale record, and says the regulation applies from a section hint alone. The US Privacy crate constructs a string from a Global Privacy Control header in a US state under the declared defaults and records it as a policy default.
- The policy parses `signals.<scheme>.authoritative` for any scheme and the opt-out sources as names.
- The identity graph record writes every vouched signal under its scheme and reads a record written by an earlier build.
- The Axum adapter's integration test assembles the permissions from records built through each crate's constructor, and from real cookies and headers read through the modules, including the string the US Privacy module constructs when the GPC module is left off the list.
- The server-side auction gate runs where basic ad selection is set and fails closed where it is not.
