/// <reference types="vite/client" />

/** Build-time source imports are embedded strings, not runtime modules. */
declare module '*?inline' {
  const source: string;
  export default source;
}
