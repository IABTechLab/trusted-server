import { log } from '../core/log';

declare const permutive: {
  config: {
    advertiserApiVersion: string;
    apiHost: string;
    apiKey: string;
    apiProtocol: string;
    apiVersion: string;
    cdnBaseUrl: string;
    cdnProtocol: string;
    classificationModelsApiVersion: string;
    consentRequired: boolean;
    cookieDomain: string;
    cookieExpiry: string;
    cookieName: string;
    environment: string;
    eventsCacheLimitBytes: number;
    eventsTTLInDays: number | null;
    localStorageDebouncedKeys: string[];
    localStorageWriteDelay: number;
    localStorageWriteMaxDelay: number;
    loggingEnabled: boolean;
    metricsSamplingPercentage: number;
    permutiveDataMiscKey: string;
    permutiveDataQueriesKey: string;
    prebidAuctionsRandomDownsamplingThreshold: number;
    pxidHost: string;
    requestTimeout: number;
    sdkErrorsApiVersion: string;
    sdkType: string;
    secureSignalsApiHost: string;
    segmentSyncApiHost: string;
    sendClientErrors: boolean;
    stateNamespace: string;
    tracingEnabled: boolean;
    viewId: string;
    watson: {
      enabled: boolean;
    };
    windowKey: string;
    workspaceId: string;
  };
};

export function installPermutiveShim() {
  log.info('Installing Permutive shim - rewriting API hosts to first-party domain');

  const host = window.location.host;
  const protocol = window.location.protocol === "https:" ? "https" : "http";

  permutive.config.apiHost = host + '/permutive/api';
  permutive.config.apiProtocol = protocol;

  permutive.config.secureSignalsApiHost = host + '/permutive/secure-signal';

  permutive.config.segmentSyncApiHost = host + '/permutive/sync';

  permutive.config.cdnBaseUrl = host + '/permutive/cdn';
  permutive.config.cdnProtocol = protocol;

  log.info('Permutive shim installed', {
    apiHost: permutive.config.apiHost,
    secureSignalsApiHost: permutive.config.secureSignalsApiHost,
    segmentSyncApiHost: permutive.config.segmentSyncApiHost,
    cdnBaseUrl: permutive.config.cdnBaseUrl,
  });
}
