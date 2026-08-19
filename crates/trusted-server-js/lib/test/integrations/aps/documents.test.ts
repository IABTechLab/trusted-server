import { describe, expect, it, vi } from 'vitest';

import envelopeFixture from '../../fixtures/aps-renderer-v1.json';
import type { ApsRendererV1 } from '../../../src/core/types';
import { parseApsDocumentEnvelopeV1 } from '../../../src/core/contracts/aps_renderer';
import {
  APS_IFRAME_INNER_CSP,
  APS_IFRAME_OUTER_CSP,
  APS_PERMANENT_SANDBOX,
  APS_SCRIPT_INNER_CSP,
  APS_SCRIPT_OUTER_CSP,
  MAX_APS_CONTAINER_DOCUMENT_BYTES,
  MAX_APS_INNER_DOCUMENT_BYTES,
  generateApsDataDocumentsV1,
} from '../../../src/integrations/aps/documents';

const publisherOrigin = 'https://publisher.example';
const creativeOrigin = 'https://creative.example';
const bootstrapNonce = `b1_${'b'.repeat(22)}`;
const rendererNonce = `n1_${'n'.repeat(22)}`;

function encodeEnvelope(value: unknown): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function renderer(tagType: 'iframe' | 'script' = 'iframe'): ApsRendererV1 {
  const envelope = structuredClone(envelopeFixture);
  envelope.seatbid[0]!.bid[0]!.ext.tagtype = tagType;
  const bid = envelope.seatbid[0]!.bid[0]!;
  return {
    type: 'aps',
    version: 1,
    accountId: 'document-account',
    bidId: bid.id,
    creativeId: 'document-creative',
    tagType,
    creativeUrl: bid.ext.creativeurl,
    width: bid.w,
    height: bid.h,
    aaxResponse: encodeEnvelope(envelope),
  };
}

function decodeDataDocument(url: string, nonce: string): string {
  const prefix = 'data:text/html;charset=utf-8,';
  const suffix = `#${nonce}`;
  expect(url.startsWith(prefix)).toBe(true);
  expect(url.endsWith(suffix)).toBe(true);
  return decodeURIComponent(url.slice(prefix.length, -suffix.length));
}

function inlineScript(documentSource: string): string {
  const start = documentSource.indexOf('<script>');
  const end = documentSource.lastIndexOf('</script>');
  if (start < 0 || end <= start) throw new Error('document should contain one inline script');
  return documentSource.slice(start + '<script>'.length, end);
}

function executeDocumentScript(
  documentSource: string,
  bindings: Readonly<Record<string, unknown>>
): void {
  const names = Object.keys(bindings);
  const run = new Function(...names, inlineScript(documentSource));
  run(...names.map((name) => bindings[name]));
}

describe('APS data documents', () => {
  it('generates detached bounded iframe documents with exact nonces and CSPs', () => {
    const documents = generateApsDataDocumentsV1({
      renderer: renderer(),
      publisherOrigin,
      bootstrapNonce,
      rendererNonce,
    });

    expect(documents).toBeDefined();
    expect(Object.isFrozen(documents)).toBe(true);
    expect(documents?.sandbox).toBe(APS_PERMANENT_SANDBOX);
    expect(documents?.outerCsp).toBe(APS_IFRAME_OUTER_CSP(publisherOrigin, creativeOrigin));
    expect(documents?.innerCsp).toBe(APS_IFRAME_INNER_CSP(publisherOrigin, creativeOrigin));
    expect(decodeDataDocument(documents!.outerUrl, bootstrapNonce)).toBe(documents?.outerDocument);
    expect(decodeDataDocument(documents!.innerUrl, rendererNonce)).toBe(documents?.innerDocument);
    expect(new TextEncoder().encode(documents?.outerDocument).byteLength).toBeLessThanOrEqual(
      MAX_APS_CONTAINER_DOCUMENT_BYTES
    );
    expect(new TextEncoder().encode(documents?.innerDocument).byteLength).toBeLessThanOrEqual(
      MAX_APS_INNER_DOCUMENT_BYTES
    );
    expect(documents?.outerDocument).toContain(bootstrapNonce);
    expect(documents?.outerDocument).toContain(rendererNonce);
    expect(documents?.outerDocument).toContain(encodeURIComponent(documents!.innerDocument));
    expect(documents?.outerDocument).not.toContain('fictional-bid-1');
    expect(documents?.outerDocument).not.toContain('/render');
    expect(documents?.outerDocument).not.toMatch(/__TS_[A-Z0-9_]+__/);
    expect(documents?.innerDocument).not.toMatch(/__TS_[A-Z0-9_]+__/);
    expect(documents?.outerCsp).not.toContain(
      `script-src 'unsafe-inline' ${publisherOrigin} ${creativeOrigin}`
    );
  });

  it('adds the validated creative origin to script-src only for script creatives', () => {
    const documents = generateApsDataDocumentsV1({
      renderer: renderer('script'),
      publisherOrigin,
      bootstrapNonce,
      rendererNonce,
    });

    expect(documents?.outerCsp).toBe(APS_SCRIPT_OUTER_CSP(publisherOrigin, creativeOrigin));
    expect(documents?.innerCsp).toBe(APS_SCRIPT_INNER_CSP(publisherOrigin, creativeOrigin));
    expect(documents?.outerCsp).toContain(
      `script-src 'unsafe-inline' ${publisherOrigin} ${creativeOrigin}`
    );
    expect(documents?.innerCsp).toContain(
      `script-src 'unsafe-inline' ${publisherOrigin} ${creativeOrigin}`
    );
  });

  it('binds one exact inner window and transfers opposite channel endpoints', () => {
    const documents = generateApsDataDocumentsV1({
      renderer: renderer(),
      publisherOrigin,
      bootstrapNonce,
      rendererNonce,
    });
    if (!documents) throw new Error('expected generated documents');
    let receive: ((event: Record<string, unknown>) => void) | undefined;
    const innerWindow = { postMessage: vi.fn() };
    const frame = {
      contentWindow: innerWindow,
      setAttribute: vi.fn(),
      src: '',
    };
    const parentWindow = { postMessage: vi.fn() };
    const port1 = { close: vi.fn() };
    const port2 = { close: vi.fn() };
    class FakeMessageChannel {
      readonly port1 = port1;
      readonly port2 = port2;
    }
    executeDocumentScript(documents.outerDocument, {
      document: {
        createElement: () => frame,
        body: { appendChild: vi.fn() },
      },
      location: { hash: `#${bootstrapNonce}`, pathname: '', search: '' },
      history: { replaceState: vi.fn() },
      parent: parentWindow,
      MessageChannel: FakeMessageChannel,
      TextEncoder,
      addEventListener: (_type: string, listener: (event: Record<string, unknown>) => void) => {
        receive = listener;
      },
      removeEventListener: vi.fn(),
    });

    expect(frame.src).toBe(documents.innerUrl);
    expect(frame.setAttribute).toHaveBeenCalledWith('sandbox', APS_PERMANENT_SANDBOX);
    receive?.({
      source: {},
      origin: 'null',
      ports: [],
      data: JSON.stringify({ message: 'TS APS Inner Ready', version: 1, rendererNonce }),
    });
    expect(parentWindow.postMessage).not.toHaveBeenCalled();
    receive?.({
      source: innerWindow,
      origin: 'null',
      ports: [],
      data: JSON.stringify({ message: 'TS APS Inner Ready', version: 1, rendererNonce }),
    });
    expect(innerWindow.postMessage).toHaveBeenCalledWith(
      JSON.stringify({ message: 'TS APS Inner Bind', version: 1, rendererNonce }),
      '*',
      [port1]
    );
    expect(parentWindow.postMessage).toHaveBeenCalledWith(
      JSON.stringify({
        message: 'TS APS Container Ready',
        version: 1,
        bootstrapNonce,
        rendererNonce,
      }),
      '*',
      [port2]
    );
    receive?.({
      source: innerWindow,
      origin: 'null',
      ports: [],
      data: JSON.stringify({ message: 'TS APS Inner Ready', version: 1, rendererNonce }),
    });
    expect(parentWindow.postMessage).toHaveBeenCalledOnce();
  });

  it('accepts one exact port envelope and completes only after runner load and callback', async () => {
    const source = renderer();
    const documents = generateApsDataDocumentsV1({
      renderer: source,
      publisherOrigin,
      bootstrapNonce,
      rendererNonce,
    });
    if (!documents) throw new Error('expected generated documents');
    let receive: ((event: Record<string, unknown>) => void) | undefined;
    let appendedScript: Record<string, unknown> | undefined;
    const parentWindow = { postMessage: vi.fn() };
    const port = {
      close: vi.fn(),
      onmessage: undefined as ((event: { data: unknown }) => void) | null | undefined,
      onmessageerror: undefined as (() => void) | undefined,
      postMessage: vi.fn(),
      start: vi.fn(),
    };
    const innerWindow: { _aps?: Map<string, { queue: CustomEvent[] }> } = {};
    executeDocumentScript(documents.innerDocument, {
      window: innerWindow,
      document: {
        createElement: () => ({}),
        head: {
          appendChild: (script: Record<string, unknown>) => {
            appendedScript = script;
          },
        },
      },
      location: { hash: `#${rendererNonce}`, pathname: '', search: '' },
      history: { replaceState: vi.fn() },
      parent: parentWindow,
      TextEncoder,
      TextDecoder,
      URL,
      atob,
      btoa,
      addEventListener: (_type: string, listener: (event: Record<string, unknown>) => void) => {
        receive = listener;
      },
      removeEventListener: vi.fn(),
    });

    expect(parentWindow.postMessage).toHaveBeenCalledWith(
      JSON.stringify({ message: 'TS APS Inner Ready', version: 1, rendererNonce }),
      '*'
    );
    receive?.({
      source: parentWindow,
      origin: 'null',
      ports: [port],
      data: JSON.stringify({ message: 'TS APS Inner Bind', version: 1, rendererNonce }),
    });
    expect(port.start).toHaveBeenCalledOnce();
    const receiveEnvelope = port.onmessage;
    receiveEnvelope?.({
      data: { version: 1, nonce: rendererNonce, publisherOrigin, renderer: source },
    });

    expect(port.postMessage).toHaveBeenCalledWith({
      message: 'TS APS Document Accepted',
      version: 1,
      nonce: rendererNonce,
    });
    expect(appendedScript?.['src']).toBe(`${publisherOrigin}/integrations/aps/runner.js`);
    expect(appendedScript?.['crossOrigin']).toBe('anonymous');
    expect(appendedScript?.['referrerPolicy']).toBe('no-referrer');
    expect(port.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ message: 'TS APS Render Completed' })
    );
    (appendedScript?.['onload'] as (() => void) | undefined)?.();
    expect(port.postMessage).toHaveBeenCalledWith({
      message: 'TS APS Runner Loaded',
      version: 1,
      nonce: rendererNonce,
    });
    const event = innerWindow._aps?.get(source.accountId)?.queue[0];
    expect(event?.type).toBe('prebid/creative/render');
    (event?.detail as { resolve?: () => void } | undefined)?.resolve?.();
    await Promise.resolve();
    expect(port.postMessage).toHaveBeenCalledWith({
      message: 'TS APS Render Completed',
      version: 1,
      nonce: rendererNonce,
    });
    expect(port.close).toHaveBeenCalledOnce();
  });

  it('fails closed for crossed capabilities, unsafe origins, and invalid descriptors', () => {
    const valid = renderer();
    for (const candidate of [
      { renderer: valid, publisherOrigin, bootstrapNonce: rendererNonce, rendererNonce },
      { renderer: valid, publisherOrigin, bootstrapNonce, rendererNonce: bootstrapNonce },
      {
        renderer: valid,
        publisherOrigin: 'http://publisher.example',
        bootstrapNonce,
        rendererNonce,
      },
      {
        renderer: { ...valid, creativeUrl: publisherOrigin },
        publisherOrigin,
        bootstrapNonce,
        rendererNonce,
      },
      {
        renderer: { ...valid, unknown: true },
        publisherOrigin,
        bootstrapNonce,
        rendererNonce,
      },
    ]) {
      expect(generateApsDataDocumentsV1(candidate)).toBeUndefined();
    }
  });
});

describe('APS document envelope', () => {
  it('copies and freezes one exact nonce- and origin-bound envelope', () => {
    const source = renderer();
    const parsed = parseApsDocumentEnvelopeV1(
      { version: 1, nonce: rendererNonce, publisherOrigin, renderer: source },
      rendererNonce,
      publisherOrigin
    );

    expect(parsed).toEqual({ version: 1, nonce: rendererNonce, publisherOrigin, renderer: source });
    expect(Object.isFrozen(parsed)).toBe(true);
    expect(Object.isFrozen(parsed?.renderer)).toBe(true);
    expect(parsed?.renderer).not.toBe(source);
  });

  it('rejects unknown keys, accessors, wrong prototypes, nonces, origins, and descriptors', () => {
    const source = renderer();
    const accessor = { version: 1, nonce: rendererNonce, publisherOrigin } as Record<
      string,
      unknown
    >;
    Object.defineProperty(accessor, 'renderer', {
      enumerable: true,
      get: () => source,
    });
    for (const candidate of [
      { version: 1, nonce: rendererNonce, publisherOrigin, renderer: source, extra: true },
      Object.assign(Object.create({ inherited: true }), {
        version: 1,
        nonce: rendererNonce,
        publisherOrigin,
        renderer: source,
      }),
      accessor,
      { version: 1, nonce: `n1_${'x'.repeat(22)}`, publisherOrigin, renderer: source },
      {
        version: 1,
        nonce: rendererNonce,
        publisherOrigin: 'https://wrong.example',
        renderer: source,
      },
      { version: 1, nonce: rendererNonce, publisherOrigin, renderer: { ...source, version: 2 } },
    ]) {
      expect(parseApsDocumentEnvelopeV1(candidate, rendererNonce, publisherOrigin)).toBeUndefined();
    }
  });
});
