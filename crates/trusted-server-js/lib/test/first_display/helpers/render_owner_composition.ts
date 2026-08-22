import type { FirstDisplayApsProtocolV1 } from '../../../src/first_display/leaf/aps_protocol';
import { createFirstDisplayApsRenderStrategy } from '../../../src/first_display/render_bridge';
import {
  createFirstDisplayRenderJournal,
  type FirstDisplayRenderOwnerOptionsV1,
} from '../../../src/first_display/render_journal';

type TestRenderBridgeOptions = FirstDisplayRenderOwnerOptionsV1 &
  Readonly<{ getAps: () => FirstDisplayApsProtocolV1 | undefined }>;

/** Compose the production owner and APS capabilities without creating a production graph seam. */
export function createTestFirstDisplayRenderBridge(options: TestRenderBridgeOptions) {
  const { getAps, ...ownerOptions } = options;
  const aps = getAps();
  const strategy = aps ? createFirstDisplayApsRenderStrategy(ownerOptions, aps) : undefined;
  return createFirstDisplayRenderJournal(ownerOptions, strategy);
}
