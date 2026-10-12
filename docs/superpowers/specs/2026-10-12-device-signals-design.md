# Design Spec: A device module may score a person and describe the device

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#23, ready for review, answering IABTechLab/trusted-server#1286. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-07-30-pluggable-providers-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#23
**Last updated:** 2026-10-12

> **Why this spec exists.** A device module answers one question, whether the request looks like a browser, and the Edge Cookie gate reads the answer.

## 1. Problem

A device module answers one question, whether the request looks like a browser, and the Edge Cookie gate reads the answer. A module that reaches a detection service knows far more, including how confident it is that a person is looking at the page and what the device is, and a bidder prices inventory on the device it is told about. Core has no place for either answer, so a module's knowledge stops at the gate and a bid request leaves without the device.

## 2. Design

Two answers a device module may give beyond the coarse browser-or-bot signals the Edge Cookie gate uses.

`DeviceSignals::human_confidence`, one of `High`, `Medium`, `Low` and `Unknown`, is how confident the module is that a person is looking at the page. A module that scores the request writes it and every other module leaves it `Unknown`, which is kept apart from `Low` because a request alone cannot establish a person, so a first-time visitor is never called a machine. Nothing reads it yet. It is written where the device module runs and is not stored in the identity graph with the other signals. Redirect middlewares that treat `Low`, `Medium` and `High` differently follow once this set of changes has landed, and are kept out of it so the set does not grow. `looks_like_browser` gates the Edge Cookie as it did.

`DeviceAttributes` is what a bidder is told about the device, being the OpenRTB device type, make, model, operating system and version, and screen size, every field optional because every source is partial. `DeviceModule::advertising_attributes` answers it from the module call `detect` is handed, with a default of nothing so no existing module changes. After each auction request is built, on the auction endpoint and the publisher's server-side path and not on the consent-denied path, which makes no outbound call, `attach_device_attributes` asks the selected module and puts the answer on the request's device object, which the OpenRTB request carries as `devicetype`, `make`, `model`, `os`, `osv`, `w` and `h`, each absent where nothing resolved it. A module that resolves nothing adds no device object, because an absent object and an empty one say the same to a bidder and the absent one is smaller. `device_type_from_name` maps a detection name to the OpenRTB code and maps nothing for a name it does not know, because a wrong type is worse for a bidder than an absent one.

The attributes are kept apart from the signals on purpose. The signals other than the score answer whether the request looks like a browser and are stored in the identity graph. The attributes answer what a bidder prices and are stored nowhere.

**Why.** A device module that reaches a detection service knows far more than whether the request looks like a browser, and a bidder prices inventory on the device it is told about. Without a place for that answer in core, a module's knowledge stops at the gate, and a deployment that wants its bid requests to carry the device has to patch core.

## 3. What is not in this change

- A module that scores a person or resolves a device. The stock build's device modules read the User-Agent and the host's signals, and answer `Unknown` and nothing.

## 4. Acceptance

The seam probe's device module scores the request `Medium` and answers fixed attributes naming a tablet. The Axum adapter's test resolves it through `[device] module` and drives both answers through the call core hands a device module, and reads back the confidence, the attributes and that a tablet is OpenRTB device type 5.

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 7 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
