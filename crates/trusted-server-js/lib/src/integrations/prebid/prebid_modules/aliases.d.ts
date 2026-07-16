declare module 'tsjs-prebid/liveIntentIdSystemStandard';

declare module 'tsjs-prebid/prebidGlobal' {
  export function registerModule(name: string): void;
}

declare module 'prebid.js/src/adRendering.js' {
  export function markBidAsRendered(bid: unknown): void;
  export function markWinner(bid: unknown): void;
}
