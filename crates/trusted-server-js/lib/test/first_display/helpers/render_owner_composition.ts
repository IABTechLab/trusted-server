import type { FirstDisplayApsPolicyV1 } from '../../../src/first_display/leaf/aps_protocol';
import { createFirstDisplayApsRenderStrategy } from '../../../src/first_display/render_bridge';
import {
  createFirstDisplayRenderJournal,
  type FirstDisplayRenderOwnerOptionsV1,
} from '../../../src/first_display/render_journal';

type TestRenderBridgeOptions = Readonly<{
  browser: FirstDisplayRenderOwnerOptionsV1[0];
  clearTimer: FirstDisplayRenderOwnerOptionsV1[1];
  createChannel: FirstDisplayRenderOwnerOptionsV1[2];
  document: FirstDisplayRenderOwnerOptionsV1[3];
  fillRandom: FirstDisplayRenderOwnerOptionsV1[4];
  now: FirstDisplayRenderOwnerOptionsV1[5];
  onNativeMutation?: NonNullable<FirstDisplayRenderOwnerOptionsV1[6]>;
  setTimer: FirstDisplayRenderOwnerOptionsV1[7];
  getAps: () => FirstDisplayApsPolicyV1 | undefined;
}>;

/** Compose the production owner and APS capabilities without creating a production graph seam. */
export function createTestFirstDisplayRenderBridge(options: TestRenderBridgeOptions) {
  const { getAps, ...ownerOptions } = options;
  const capabilities: FirstDisplayRenderOwnerOptionsV1 = Object.freeze([
    ownerOptions.browser,
    ownerOptions.clearTimer,
    ownerOptions.createChannel,
    ownerOptions.document,
    ownerOptions.fillRandom,
    ownerOptions.now,
    ownerOptions.onNativeMutation,
    ownerOptions.setTimer,
  ]);
  const aps = getAps();
  const strategy = aps ? createFirstDisplayApsRenderStrategy(capabilities, aps) : undefined;
  return createFirstDisplayRenderJournal(capabilities, strategy);
}
