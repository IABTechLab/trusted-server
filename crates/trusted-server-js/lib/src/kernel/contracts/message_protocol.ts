/** Every protocol literal shared by the version-1 browser message channels. */
export const TSJS_MESSAGE_PROTOCOL_V1 = Object.freeze({
  version: 1 as const,
  rendererVersion: '3' as const,
  message: Object.freeze({
    prebidRequest: 'Prebid Request' as const,
    prebidResponse: 'Prebid Response' as const,
    ownerRegister: 'TS Render Owner Register' as const,
    ownerRegistered: 'TS Render Owner Registered' as const,
    ownerRefused: 'TS Render Owner Refused' as const,
    apsStart: 'TS APS Start' as const,
    admStart: 'TS ADM Start' as const,
    ownerInserted: 'TS Owner Inserted' as const,
    ownerSettled: 'TS Owner Settled' as const,
    admLoaded: 'TS ADM Loaded' as const,
    admFailed: 'TS ADM Failed' as const,
    apsDocumentAccepted: 'TS APS Document Accepted' as const,
    apsRunnerLoaded: 'TS APS Runner Loaded' as const,
    apsRenderCompleted: 'TS APS Render Completed' as const,
    apsRenderFailed: 'TS APS Render Failed' as const,
  }),
  status: Object.freeze({ ready: 'ready' as const, refused: 'refused' as const }),
  kind: Object.freeze({ aps: 'aps' as const, adm: 'adm' as const }),
  outcome: Object.freeze({
    accepted: 'accepted' as const,
    failed: 'failed' as const,
    cancelled: 'cancelled' as const,
  }),
  runnerFailure: Object.freeze({
    descriptorInvalid: 'descriptor_invalid' as const,
    runnerNoLoad: 'runner_no_load' as const,
    runnerFailed: 'runner_failed' as const,
  }),
  cancellation: Object.freeze({
    callerAborted: 'caller_aborted' as const,
    superseded: 'superseded' as const,
    navigationDisposed: 'navigation_disposed' as const,
  }),
});
