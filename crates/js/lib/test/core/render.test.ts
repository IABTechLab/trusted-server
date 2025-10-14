import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('render', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
  });

  it('injects creative into existing slot (via iframe)', async () => {
    const { renderCreativeIntoSlot } = await import('../../src/core/render');
    const div = document.createElement('div');
    div.id = 'slotA';
    document.body.appendChild(div);
    renderCreativeIntoSlot('slotA', '<span>ad</span>');
    const iframe = document.getElementById('slotA')!.querySelector('iframe');
    expect(iframe).toBeTruthy();
  });
});
