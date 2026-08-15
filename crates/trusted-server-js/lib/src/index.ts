// Barrel re-export for convenience and tests.
// At build time, each module (core + integrations) is built as a separate IIFE
// by build-all.mjs. The Rust server concatenates the enabled modules at runtime.
export type {
  AdUnit,
  GptDiagnosticsApi,
  GptDiagnosticsExportV1,
  GptDiagnosticsRequestCycle,
  TsjsApi,
} from './core/types';
export { log } from './core/log';
