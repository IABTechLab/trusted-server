// Bounded ad-template evidence collector, injected before publisher scripts run.
//
// This body runs inside an IIFE that defines `__TS_CONFIG` (the configured div
// prefixes). It records evidence into `window.__tsAdTemplateEvidence`
// and never captures page HTML, cookies, storage, request bodies, or arbitrary DOM.
// It always calls original page functions with unchanged arguments and never
// spoofs the browser automation flag.

const __ts_config = typeof __TS_CONFIG === "object" && __TS_CONFIG ? __TS_CONFIG : {}
const __ts_prefixes = Array.isArray(__ts_config.div_prefixes) ? __ts_config.div_prefixes : []

const __ts_ev = (window.__tsAdTemplateEvidence = window.__tsAdTemplateEvidence || {
  dom_ids: [],
  gpt_slots: [],
  aps_calls: [],
  warnings: []
})

const __ts_phase = () => (window.__tsScrollPhase ? "scroll" : "initial_load")

// Hard cap per evidence list so a hostile page cannot grow the store without
// bound; the page controls how many slots/elements/warnings it produces.
const __ts_max_entries = 128
const __ts_max_string_length = 512
const __ts_wrapped_googletags = new WeakSet()

function __ts_text(value) {
  return String(value).slice(0, __ts_max_string_length)
}

// Truncation has to be visible: surplus configured slots classify Missing, and
// `--strict` counts that, so a silent drop is indistinguishable from real drift.
let __ts_truncated = false
function __ts_push(list, entry) {
  if (list.length < __ts_max_entries) {
    list.push(entry)
    return
  }
  if (__ts_truncated) return
  __ts_truncated = true
  if (__ts_ev.warnings.length < __ts_max_entries) {
    __ts_ev.warnings.push({
      code: "evidence_truncated",
      message: "an evidence list hit the " + __ts_max_entries + "-entry cap; results are incomplete"
    })
  }
}

function __ts_warn(code, error) {
  __ts_push(__ts_ev.warnings, { code, message: __ts_text(error) })
}

// GPT sizes reach Rust as u32 pairs, so anything non-integral (fluid slots,
// NaN, negative or fractional dimensions) must be dropped here — a single bad
// pair would fail deserialization of the whole evidence payload and discard
// every other slot's otherwise valid evidence.
function __ts_size_pair(width, height) {
  if (!Number.isInteger(width) || !Number.isInteger(height)) return null
  if (width < 0 || height < 0 || width > 4294967295 || height > 4294967295) return null
  return [width, height]
}

function __ts_warn_ignored_size(width, height) {
  const numeric = Number.isInteger(width) && Number.isInteger(height)
  const outOfRange =
    numeric && (width < 0 || height < 0 || width > 4294967295 || height > 4294967295)
  __ts_push(__ts_ev.warnings, {
    code: outOfRange ? "size_out_of_range" : "fluid_size_ignored",
    message: outOfRange ? "GPT size outside u32 range ignored" : "non-integer GPT size ignored"
  })
}

function __ts_normalize_sizes(sizes) {
  const out = []
  if (!Array.isArray(sizes)) return out
  // Accept [w, h] or [[w, h], ...]; treat numeric-leading arrays as a single pair.
  const pairs = typeof sizes[0] === "number" ? [sizes] : sizes
  for (const size of pairs) {
    if (out.length >= __ts_max_entries) break
    const pair = Array.isArray(size) ? __ts_size_pair(size[0], size[1]) : null
    if (pair) {
      out.push(pair)
    } else {
      __ts_warn_ignored_size(
        Array.isArray(size) ? size[0] : undefined,
        Array.isArray(size) ? size[1] : undefined
      )
    }
  }
  return out
}

function __ts_record_define_slot(adUnitPath, sizes, divId) {
  __ts_push(__ts_ev.gpt_slots, {
    gam_unit_path: __ts_text(adUnitPath),
    div_id: __ts_text(divId),
    sizes: __ts_normalize_sizes(sizes),
    phase: __ts_phase()
  })
}

function __ts_wrap_googletag(googletag) {
  if (!googletag || (typeof googletag !== "object" && typeof googletag !== "function")) {
    return googletag
  }
  if (__ts_wrapped_googletags.has(googletag)) return googletag
  __ts_wrapped_googletags.add(googletag)
  // Wrap defineSlot so both direct calls and calls dispatched from the cmd queue
  // are recorded (queued callbacks call this same wrapped function).
  const originalDefineSlot = googletag.defineSlot
  if (typeof originalDefineSlot === "function") {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(googletag, "defineSlot")
      Object.defineProperty(googletag, "defineSlot", {
        configurable: true,
        enumerable: descriptor ? descriptor.enumerable : true,
        writable: true,
        value: function (adUnitPath, sizes, divId) {
          const slot = originalDefineSlot.apply(this, arguments)
          try {
            __ts_record_define_slot(adUnitPath, sizes, divId)
          } catch (error) {
            __ts_warn("define_slot_capture_failed", error)
          }
          return slot
        }
      })
    } catch (error) {
      __ts_warn("define_slot_wrap_failed", error)
    }
  }
  return googletag
}

// Wrap an existing global or intercept a later assignment of it.
function __ts_install(name, wrap) {
  if (window[name]) {
    try {
      wrap(window[name])
    } catch (error) {
      __ts_warn(name + "_wrap_failed", error)
    }
    return
  }
  let internal
  Object.defineProperty(window, name, {
    configurable: true,
    // A real `window.googletag` is an ordinary enumerable global; matching that
    // keeps `Object.keys(window)` identical with and without the collector.
    enumerable: true,
    get() {
      return internal
    },
    set(value) {
      internal = value
      try {
        internal = wrap(value)
      } catch (error) {
        __ts_warn(name + "_wrap_failed", error)
      }
    }
  })
}

__ts_install("googletag", __ts_wrap_googletag)

// On-demand DOM + getSlots scrape, invoked by the collector after settle/scroll.
window.__tsCollectAdTemplateEvidence = function () {
  try {
    const seen = new Set(__ts_ev.dom_ids.map((entry) => entry.dom_id))
    for (const element of document.querySelectorAll("[id]")) {
      const id = __ts_text(element.id)
      if (id.endsWith("-container")) continue
      if (__ts_prefixes.some((prefix) => id.startsWith(prefix)) && !seen.has(id)) {
        __ts_push(__ts_ev.dom_ids, { dom_id: id, phase: __ts_phase() })
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
            if (sizes.length >= __ts_max_entries) break
            let pair = null
            if (
              size &&
              typeof size.getWidth === "function" &&
              typeof size.getHeight === "function"
            ) {
              // A fluid GPT size answers getWidth()/getHeight() with a
              // non-numeric value rather than throwing.
              pair = __ts_size_pair(size.getWidth(), size.getHeight())
            } else if (Array.isArray(size)) {
              pair = __ts_size_pair(size[0], size[1])
            }
            if (pair) {
              sizes.push(pair)
            } else {
              const width =
                size && typeof size.getWidth === "function"
                  ? size.getWidth()
                  : Array.isArray(size)
                    ? size[0]
                    : undefined
              const height =
                size && typeof size.getHeight === "function"
                  ? size.getHeight()
                  : Array.isArray(size)
                    ? size[1]
                    : undefined
              __ts_warn_ignored_size(width, height)
            }
          }
          const exists = __ts_ev.gpt_slots.some(
            (entry) => entry.gam_unit_path === __ts_text(path) && entry.div_id === __ts_text(divId)
          )
          if (!exists) {
            __ts_push(__ts_ev.gpt_slots, {
              gam_unit_path: __ts_text(path),
              div_id: __ts_text(divId),
              sizes,
              phase: __ts_phase()
            })
          }
        } catch (error) {
          __ts_warn("gpt_scrape_failed", error)
        }
      }
    }
  } catch (error) {
    __ts_warn("collect_failed", error)
  }
  return __ts_ev
}
