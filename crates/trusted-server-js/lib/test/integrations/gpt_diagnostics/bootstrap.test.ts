import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { describe, expect, it } from 'vitest';

const bootstrapPath = resolve(
  process.cwd(),
  '../../trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js'
);
const bootstrapSource = readFileSync(bootstrapPath, 'utf8');
describe('GPT diagnostics activation ownership', () => {
  it('leaves no browser-owned query, storage, history, or activation-flag bootstrap', () => {
    expect(bootstrapSource).not.toMatch(/ts_console/);
    expect(bootstrapSource).not.toMatch(/sessionStorage|localStorage/);
    expect(bootstrapSource).not.toMatch(/replaceState/);
    expect(bootstrapSource).not.toMatch(/__tsjs_gpt_diagnostics_active/);
  });
});
