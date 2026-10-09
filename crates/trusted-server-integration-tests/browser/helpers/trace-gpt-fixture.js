;(() => {
  const slots = new Map()
  const listeners = new Map()
  const requests = []
  let initialLoadDisabled = false

  function emit(name, slot, facts = {}) {
    for (const listener of listeners.get(name) || [])
      listener({ slot, ...facts })
  }

  const service = {
    getSlots: () => [...slots.values()],
    getTargeting: () => [],
    getTargetingKeys: () => [],
    setTargeting() {
      return service
    },
    clearTargeting() {
      return service
    },
    enableSingleRequest() {},
    disableInitialLoad() {
      initialLoadDisabled = true
    },
    isInitialLoadDisabled: () => initialLoadDisabled,
    addEventListener(name, callback) {
      const callbacks = listeners.get(name) || []
      callbacks.push(callback)
      listeners.set(name, callbacks)
      return service
    },
    removeEventListener(name, callback) {
      listeners.set(
        name,
        (listeners.get(name) || []).filter((entry) => entry !== callback)
      )
      return service
    },
    refresh(selected = [...slots.values()]) {
      for (const slot of selected) {
        if (slots.get(slot.getSlotElementId()) !== slot) continue
        requests.push({
          slot,
          targeting: Object.fromEntries(
            slot.getTargetingKeys().map((key) => [key, slot.getTargeting(key)])
          ),
        })
        emit('slotRequested', slot)
      }
    },
  }

  window.googletag = {
    apiReady: true,
    pubadsReady: true,
    cmd: {
      push(...callbacks) {
        callbacks.forEach((callback) => callback())
        return callbacks.length
      },
    },
    pubads: () => service,
    defineSlot(path, sizes, id) {
      const targeting = new Map()
      const dimensions = typeof sizes[0] === 'number' ? [sizes] : sizes
      const slot = {
        getSlotElementId: () => id,
        getAdUnitPath: () => path,
        getSizes: () =>
          dimensions.map(([width, height]) => ({
            getWidth: () => width,
            getHeight: () => height,
          })),
        getTargeting: (key) => targeting.get(key) || [],
        getTargetingKeys: () => [...targeting.keys()],
        setTargeting(key, value) {
          targeting.set(
            key,
            (Array.isArray(value) ? value : [value]).map(String)
          )
          return slot
        },
        clearTargeting(key) {
          if (key === undefined) targeting.clear()
          else targeting.delete(key)
          return slot
        },
        updateTargetingFromMap(values) {
          for (const [key, value] of Object.entries(values)) {
            if (value === null) targeting.delete(key)
            else slot.setTargeting(key, value)
          }
          return slot
        },
        addService: () => slot,
        setConfig: () => slot,
      }
      slots.set(id, slot)
      return slot
    },
    destroySlots(selected = [...slots.values()]) {
      for (const slot of selected) slots.delete(slot.getSlotElementId())
      return true
    },
    enableServices() {},
    display(target) {
      const id =
        typeof target === 'string'
          ? target
          : target.getSlotElementId?.() || target.id
      const slot = slots.get(id)
      if (slot && !initialLoadDisabled) service.refresh([slot])
    },
    getConfig: () => ({ disableInitialLoad: initialLoadDisabled }),
    setConfig(config) {
      if (typeof config.disableInitialLoad === 'boolean')
        initialLoadDisabled = config.disableInitialLoad
    },
  }

  // This fixture supplies GPT's documented callbacks. Activation and auction
  // evidence come exclusively from the real publisher response and built TSJS.
  window.__traceGptFixture = {
    requests,
    complete(index) {
      const request = requests[index]
      if (!request) throw new Error('should complete an observed GPT request')
      const adId = request.targeting.hb_adid?.[0]
      if (adId) {
        const frame = document.createElement('iframe')
        frame.width = '300'
        frame.height = '250'
        frame.src = `https://creative.example.com/fixture-puc?adId=${encodeURIComponent(adId)}`
        document
          .getElementById(request.slot.getSlotElementId())
          .replaceChildren(frame)
      }
      emit('slotResponseReceived', request.slot)
      emit('slotRenderEnded', request.slot, {
        isEmpty: !adId,
        ...(adId ? { size: [300, 250] } : {}),
      })
    },
  }
})()
