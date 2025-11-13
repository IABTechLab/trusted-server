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



  permutive.config.apiHost = window.location.host + '/permutive/api';
  permutive.config.apiProtocol = window.location.protocol === "https:" ? "https" : "http";

  permutive.config.secureSignalsApiHost = window.location.host + '/permutive/secure-signal';
}
