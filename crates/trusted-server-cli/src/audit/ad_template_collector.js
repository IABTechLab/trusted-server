// Read-only ad-template evidence collector, injected before publisher scripts run.
//
// This body runs inside an IIFE that defines `__TS_CONFIG` (configured div
// prefixes + APS slot IDs). It records evidence into `window.__tsAdTemplateEvidence`
// and never captures page HTML, cookies, storage, request bodies, or arbitrary DOM.
// It always calls original page functions with unchanged arguments and never
// spoofs the browser automation flag.

const __ts_config = typeof __TS_CONFIG === "object" && __TS_CONFIG ? __TS_CONFIG : {}
const __ts_prefixes = Array.isArray(__ts_config.div_prefixes) ? __ts_config.div_prefixes : []

const __ts_ev = (window.__tsAdTemplateEvidence = window.__tsAdTemplateEvidence || {
  dom_ids: [],
  gpt_slots: [],
  aps_calls: [],
  warnings: [],
})

const __ts_phase = () => (window.__tsScrollPhase ? "scroll" : "initial_load")

function __ts_normalize_sizes(sizes) {
  const out = []
  if (!Array.isArray(sizes)) return out
  // Accept [w, h] or [[w, h], ...]; treat numeric-leading arrays as a single pair.
  const pairs = typeof sizes[0] === "number" ? [sizes] : sizes
  for (const size of pairs) {
    if (Array.isArray(size) && typeof size[0] === "number" && typeof size[1] === "number") {
      out.push([size[0], size[1]])
    } else {
      __ts_ev.warnings.push({
        code: "fluid_size_ignored",
        message: "non-numeric GPT size ignored",
      })
    }
  }
  return out
}

function __ts_record_define_slot(adUnitPath, sizes, divId) {
  __ts_ev.gpt_slots.push({
    gam_unit_path: String(adUnitPath),
    div_id: String(divId),
    sizes: __ts_normalize_sizes(sizes),
    phase: __ts_phase(),
  })
}

function __ts_wrap_googletag(googletag) {
  if (!googletag || googletag.__tsWrapped) return googletag
  googletag.__tsWrapped = true
  googletag.cmd = googletag.cmd || []
  // Wrap cmd.push without changing callback order (pass-through to the original).
  const originalPush = googletag.cmd.push.bind(googletag.cmd)
  googletag.cmd.push = function (callback) {
    return originalPush(callback)
  }
  // Wrap defineSlot so both direct calls and calls dispatched from the cmd queue
  // are recorded (queued callbacks call this same wrapped function).
  const originalDefineSlot = googletag.defineSlot
  if (typeof originalDefineSlot === "function") {
    googletag.defineSlot = function (adUnitPath, sizes, divId) {
      const slot = originalDefineSlot.apply(this, arguments)
      try {
        __ts_record_define_slot(adUnitPath, sizes, divId)
      } catch (error) {
        __ts_ev.warnings.push({ code: "define_slot_capture_failed", message: String(error) })
      }
      return slot
    }
  }
  return googletag
}

function __ts_wrap_apstag(apstag) {
  if (!apstag || apstag.__tsWrapped) return apstag
  apstag.__tsWrapped = true
  const originalFetchBids = apstag.fetchBids
  if (typeof originalFetchBids === "function") {
    apstag.fetchBids = function (config, callback) {
      try {
        const slots = (config && config.slots) || []
        for (const slot of slots) {
          __ts_ev.aps_calls.push({
            slot_id: String(slot.slotID || slot.slotName || ""),
            sizes: __ts_normalize_sizes(slot.sizes),
            phase: __ts_phase(),
          })
        }
      } catch (error) {
        __ts_ev.warnings.push({ code: "aps_capture_failed", message: String(error) })
      }
      return originalFetchBids.apply(this, arguments)
    }
  }
  return apstag
}

// Wrap an existing global or intercept a later assignment of it.
function __ts_install(name, wrap) {
  if (window[name]) {
    wrap(window[name])
    return
  }
  let internal
  Object.defineProperty(window, name, {
    configurable: true,
    get() {
      return internal
    },
    set(value) {
      internal = wrap(value)
    },
  })
}

__ts_install("googletag", __ts_wrap_googletag)
__ts_install("apstag", __ts_wrap_apstag)

// On-demand DOM + getSlots scrape, invoked by the collector after settle/scroll.
window.__tsCollectAdTemplateEvidence = function () {
  try {
    const seen = new Set(__ts_ev.dom_ids.map((entry) => entry.dom_id))
    for (const element of document.querySelectorAll("[id]")) {
      const id = element.id
      if (id.endsWith("-container")) continue
      if (__ts_prefixes.some((prefix) => id.startsWith(prefix)) && !seen.has(id)) {
        __ts_ev.dom_ids.push({ dom_id: id, phase: __ts_phase() })
        seen.add(id)
      }
    }
    const googletag = window.googletag
    if (googletag && typeof googletag.pubads === "function") {
      const pubads = googletag.pubads()
      const slots = typeof pubads.getSlots === "function" ? pubads.getSlots() : []
      for (const slot of slots) {
        try {
          const path = typeof slot.getAdUnitPath === "function" ? slot.getAdUnitPath() : ""
          const divId = typeof slot.getSlotElementId === "function" ? slot.getSlotElementId() : ""
          const rawSizes = typeof slot.getSizes === "function" ? slot.getSizes() : []
          const sizes = []
          for (const size of rawSizes) {
            if (size && typeof size.getWidth === "function") {
              sizes.push([size.getWidth(), size.getHeight()])
            } else if (Array.isArray(size) && typeof size[0] === "number") {
              sizes.push([size[0], size[1]])
            }
          }
          const exists = __ts_ev.gpt_slots.some(
            (entry) => entry.gam_unit_path === String(path) && entry.div_id === String(divId)
          )
          if (!exists) {
            __ts_ev.gpt_slots.push({
              gam_unit_path: String(path),
              div_id: String(divId),
              sizes,
              phase: __ts_phase(),
            })
          }
        } catch (error) {
          __ts_ev.warnings.push({ code: "gpt_scrape_failed", message: String(error) })
        }
      }
    }
  } catch (error) {
    __ts_ev.warnings.push({ code: "collect_failed", message: String(error) })
  }
  return __ts_ev
}
