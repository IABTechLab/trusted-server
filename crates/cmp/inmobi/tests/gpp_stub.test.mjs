// Runs InMobi's GPP stub the InMobi Choice module writes into a page, in a
// small stand-in for the browser, and checks what a page and its frames see.
// Run with `node --test crates/cmp/inmobi/tests/gpp_stub.test.mjs`.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const STUB = readFileSync(new URL('../src/gpp_stub.js', import.meta.url), 'utf8');

// The sections InMobi's own tag says its prompt supports.
const SUPPORTED_APIS = [
  '2:tcfeuv2',
  '6:uspv1',
  '7:usnatv1',
  '8:usca',
  '9:usvav1',
  '10:uscov1',
  '11:usutv1',
  '12:usctv1',
];

// What ping answers while the stub is in place, for a CMP id.
function stubPing(cmpId) {
  return {
    gppVersion: '1.1',
    cmpStatus: 'stub',
    cmpDisplayStatus: 'hidden',
    signalStatus: 'not ready',
    supportedAPIs: SUPPORTED_APIS,
    cmpId,
    sectionList: [],
    applicableSections: [-1],
    gppString: '',
    parsedSections: {},
  };
}

// A window and document holding only what the stub touches, with the stub
// told `cmpId` as the middleware tells it. `body` is false for a page whose
// body has not been parsed yet.
function browser({ cmpId = 10, body = true, gpp } = {}) {
  const listeners = [];
  const appended = [];
  const timers = [];
  const window = {
    frames: {},
    addEventListener(type, listener) {
      listeners.push({ type, listener });
    },
  };
  if (gpp) {
    window.__gpp = gpp;
  }
  const document = {
    body: body
      ? {
          appendChild(frame) {
            appended.push(frame);
            window.frames[frame.name] = frame;
          },
        }
      : null,
    createElement(tag) {
      return { tag, style: {} };
    },
  };
  const setTimeout = (callback, delay, ...args) => timers.push({ callback, delay, args });
  new Function('window', 'document', 'setTimeout', `${STUB.trimEnd()}(${cmpId});`)(
    window,
    document,
    setTimeout
  );
  return { window, document, listeners, appended, timers };
}

// Posts `data` to the window from a frame, returning what the frame was sent.
function post(page, data) {
  const replies = [];
  const source = { postMessage: (message, origin) => replies.push({ message, origin }) };
  for (const { type, listener } of page.listeners) {
    if (type === 'message') {
      listener({ data, source });
    }
  }
  return replies;
}

test('ping answers at once with the CMP id the stub is told', () => {
  for (const cmpId of [10, 42]) {
    const page = browser({ cmpId });
    const answers = [];

    page.window.__gpp('ping', (data, success) => answers.push({ data, success }));

    assert.deepEqual(answers, [{ data: stubPing(cmpId), success: true }]);
    assert.deepEqual(page.window.__gpp('queue'), [], 'ping is answered, not queued');
  }
});

test('other calls are queued and handed over', () => {
  const page = browser();
  const callback = () => assert.fail('a queued call is answered by the prompt');

  page.window.__gpp('getGPPData', callback);
  page.window.__gpp('signalStatus', callback, 'parameter', '1.1');

  const queued = [
    ['getGPPData', callback],
    ['signalStatus', callback, 'parameter', '1.1'],
  ];
  assert.deepEqual(page.window.__gpp('queue'), queued);
  assert.deepEqual(page.window.__gpp(), queued);
  assert.equal(page.window.__gpp.queue, page.window.__gpp('queue'), 'kept on the stub');
});

test('a listener is registered and removed at once, with the ping data', () => {
  const page = browser({ cmpId: 42 });
  const answers = [];
  const answer = (data, success) => answers.push({ data, success });

  page.window.__gpp('addEventListener', answer);
  assert.deepEqual(answers, [
    {
      data: { eventName: 'listenerRegistered', listenerId: 1, data: true, pingData: stubPing(42) },
      success: true,
    },
  ]);
  assert.deepEqual(page.window.__gpp('events'), [{ id: 1, callback: answer, parameter: null }]);

  page.window.__gpp('removeEventListener', answer, 1);
  page.window.__gpp('removeEventListener', answer, 7);
  assert.deepEqual(answers.slice(1), [
    {
      data: { eventName: 'listenerRemoved', listenerId: 1, data: true, pingData: stubPing(42) },
      success: true,
    },
    {
      data: { eventName: 'listenerRemoved', listenerId: 7, data: false, pingData: stubPing(42) },
      success: true,
    },
  ]);
  assert.deepEqual(page.window.__gpp('events'), []);
  assert.deepEqual(page.window.__gpp('queue'), [], 'listeners are not queued');
});

test('hasSection, getSection and getField answer at once', () => {
  const page = browser();
  const answers = [];

  for (const command of ['hasSection', 'getSection', 'getField']) {
    page.window.__gpp(
      command,
      (data, success) => answers.push([command, data, success]),
      'tcfeuv2'
    );
  }

  assert.deepEqual(answers, [
    ['hasSection', false, true],
    ['getSection', null, true],
    ['getField', null, true],
  ]);
  assert.deepEqual(page.window.__gpp('queue'), []);
});

test('a frame that posts a call is answered with the call id', () => {
  const page = browser({ cmpId: 42 });

  const objectReplies = post(page, { __gppCall: { command: 'ping', callId: 'object-call' } });
  assert.deepEqual(objectReplies, [
    {
      message: {
        __gppReturn: { returnValue: stubPing(42), success: true, callId: 'object-call' },
      },
      origin: '*',
    },
  ]);

  // A frame that posts a string is answered with a string.
  const stringReplies = post(
    page,
    JSON.stringify({ __gppCall: { command: 'ping', callId: 'string-call' } })
  );
  assert.equal(stringReplies.length, 1);
  assert.equal(typeof stringReplies[0].message, 'string');
  const reply = JSON.parse(stringReplies[0].message).__gppReturn;
  assert.equal(reply.callId, 'string-call');
  assert.equal(reply.returnValue.cmpId, 42);

  // Anything else posted to the page is left alone.
  assert.deepEqual(post(page, 'not json'), []);
  assert.deepEqual(post(page, { other: true }), []);
  assert.deepEqual(post(page, { __gppCall: 'not a call' }), []);
});

test('creates the hidden __gppLocator frame that frames find the stub by', () => {
  const page = browser();

  assert.equal(page.appended.length, 1);
  assert.equal(page.appended[0].tag, 'iframe');
  assert.equal(page.appended[0].name, '__gppLocator');
  assert.equal(page.appended[0].style.cssText, 'display:none');
  assert.equal(page.timers.length, 0);
});

test('before the body exists the locator frame waits for it', () => {
  const page = browser({ body: false });

  assert.equal(page.appended.length, 0);
  assert.equal(page.timers.length, 1);
  assert.equal(page.timers[0].delay, 10);
  assert.deepEqual(page.timers[0].args, ['__gppLocator']);
});

test('a page that already has a __gpp keeps it', () => {
  const existing = () => 'own';
  const page = browser({ gpp: existing });

  assert.equal(page.window.__gpp, existing);
  assert.equal(page.window.__gpp_stub, undefined, 'nothing is defined');
  assert.equal(page.listeners.length, 0, 'no relay is added');
  assert.equal(page.appended.length, 0, 'no locator frame is added');
});
