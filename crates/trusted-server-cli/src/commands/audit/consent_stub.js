(() => {
  const tcData = {
    tcString: "",
    tcfPolicyVersion: 2,
    cmpId: 0,
    cmpVersion: 1,
    gdprApplies: false,
    eventStatus: "tcloaded",
    cmpStatus: "loaded",
    listenerId: 1,
    isServiceSpecific: true,
    useNonStandardTexts: false,
    purposeOneTreatment: false,
    publisherCC: "US",
    purpose: { consents: {}, legitimateInterests: {} },
    vendor: { consents: {}, legitimateInterests: {} },
    specialFeatureOptins: {},
  }
  for (let index = 1; index <= 10; index += 1) {
    tcData.purpose.consents[index] = true
    tcData.purpose.legitimateInterests[index] = true
  }

  const tcfapi = (command, version, callback) => {
    if (typeof callback !== "function") return
    switch (command) {
      case "ping":
        callback(
          {
            gdprApplies: false,
            cmpLoaded: true,
            cmpStatus: "loaded",
            displayStatus: "hidden",
            apiVersion: "2.0",
            cmpId: 0,
          },
          true
        )
        break
      case "addEventListener":
      case "getTCData":
        callback(tcData, true)
        break
      case "removeEventListener":
        callback(true, true)
        break
      default:
        callback(tcData, true)
    }
  }

  const uspapi = (command, version, callback) => {
    if (typeof callback !== "function") return
    callback({ version: 1, uspString: "1---" }, true)
  }

  const pin = (name, value) => {
    try {
      Object.defineProperty(window, name, {
        value,
        writable: false,
        configurable: false,
      })
    } catch (error) {
      // The page installed an earlier value; leave it untouched.
    }
  }
  pin("__tcfapi", tcfapi)
  pin("__uspapi", uspapi)
})()
