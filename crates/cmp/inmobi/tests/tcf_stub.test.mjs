// Runs the IAB TCF v2 stub the InMobi Choice module writes into a page, in a
// small stand-in for the browser, and checks what a page and its frames see.
// Run with `node --test crates/cmp/inmobi/tests/tcf_stub.test.mjs`.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const STUB = readFileSync(new URL('../src/tcf_stub.js', import.meta.url), 'utf8');

// A window and document holding only what the stub touches. `body` is false
// for a page whose body has not been parsed yet. `parent` and `top` are the
// windows above a page served inside a frame.
function browser({ body = true, tcfapi, parent, top } = {}) {
  const listeners = [];
  const appended = [];
  const timers = [];
  const window = {
    frames: {},
    addEventListener(type, listener) {
      listeners.push({ type, listener });
    },
  };
  if (tcfapi) {
    window.__tcfapi = tcfapi;
  }
  if (parent) {
    window.parent = parent;
    window.top = top ?? parent;
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
  const setTimeout = (callback, delay) => timers.push({ callback, delay });
  new Function('window', 'document', 'setTimeout', STUB)(window, document, setTimeout);
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

test('calls made before the prompt arrives are queued and handed over', () => {
  const page = browser();
  const callback = () => {};

  page.window.__tcfapi('getTCData', 2, callback);
  page.window.__tcfapi('addEventListener', 2, callback);

  assert.deepEqual(page.window.__tcfapi(), [
    ['getTCData', 2, callback],
    ['addEventListener', 2, callback],
  ]);
});

test('ping answers as a stub that has not loaded', () => {
  const page = browser();
  let answer;

  page.window.__tcfapi('ping', 2, (value) => {
    answer = value;
  });

  assert.deepEqual(answer, {
    gdprApplies: undefined,
    cmpLoaded: false,
    cmpStatus: 'stub',
    apiVersion: '2.2',
  });
  assert.deepEqual(page.window.__tcfapi(), [], 'ping is answered, not queued');
});

test('setGdprApplies sets what ping reports', () => {
  const page = browser();
  const set = [];

  page.window.__tcfapi('setGdprApplies', 2, (...args) => set.push(args), true);
  let answer;
  page.window.__tcfapi('ping', 2, (value) => {
    answer = value;
  });

  assert.deepEqual(set, [['set', true]]);
  assert.equal(answer.gdprApplies, true);

  // Anything but version 2 and a boolean is ignored.
  page.window.__tcfapi('setGdprApplies', 1, () => set.push('v1'), false);
  page.window.__tcfapi('setGdprApplies', 2, () => set.push('text'), 'false');
  page.window.__tcfapi('ping', 2, (value) => {
    answer = value;
  });
  assert.deepEqual(set, [['set', true]]);
  assert.equal(answer.gdprApplies, true);
});

test('a frame that posts a call is answered with the call id', () => {
  const page = browser();

  const objectReplies = post(page, {
    __tcfapiCall: { command: 'ping', version: 2, callId: 'object-call' },
  });
  assert.deepEqual(objectReplies, [
    {
      message: {
        __tcfapiReturn: {
          returnValue: {
            gdprApplies: undefined,
            cmpLoaded: false,
            cmpStatus: 'stub',
            apiVersion: '2.2',
          },
          success: undefined,
          callId: 'object-call',
        },
      },
      origin: '*',
    },
  ]);

  // A frame that posts a string is answered with a string.
  const stringReplies = post(
    page,
    JSON.stringify({ __tcfapiCall: { command: 'ping', version: 2, callId: 'string-call' } })
  );
  assert.equal(stringReplies.length, 1);
  assert.equal(typeof stringReplies[0].message, 'string');
  assert.equal(JSON.parse(stringReplies[0].message).__tcfapiReturn.callId, 'string-call');

  // Anything else posted to the page is left alone.
  assert.deepEqual(post(page, 'not json'), []);
  assert.deepEqual(post(page, { other: true }), []);
});

test('creates the hidden __tcfapiLocator frame that frames find the stub by', () => {
  const page = browser();

  assert.equal(page.appended.length, 1);
  assert.equal(page.appended[0].tag, 'iframe');
  assert.equal(page.appended[0].name, '__tcfapiLocator');
  assert.equal(page.appended[0].style.display, 'none');
  assert.equal(page.timers.length, 0);
});

test('before the body exists the locator frame waits for it', () => {
  const page = browser({ body: false });

  assert.equal(page.appended.length, 0);
  assert.equal(page.timers.length, 1);
  assert.equal(page.timers[0].delay, 5);
});

test('a page that already has a __tcfapi keeps it', () => {
  const existing = () => 'own';
  const page = browser({ tcfapi: existing });

  assert.equal(page.window.__tcfapi, existing);
  assert.equal(page.listeners.length, 0, 'no relay is added');
  assert.equal(page.appended.length, 0, 'no locator frame is added');
});

// A window above the page, with the frames it holds by name.
function ancestor(frames = {}) {
  const window = { frames };
  window.parent = window;
  window.top = window;
  return window;
}

test('a page inside a frame of a page that already has a CMP gets no second stub', () => {
  const top = ancestor({ __tcfapiLocator: {} });
  const middle = { frames: {}, parent: top, top };
  const page = browser({ parent: middle, top });

  assert.equal(page.window.__tcfapi, undefined, 'no __tcfapi is defined');
  assert.equal(page.listeners.length, 0, 'no relay is added');
  assert.equal(page.appended.length, 0, 'no locator frame is added');
  assert.equal(page.timers.length, 0, 'no locator frame is waited for');
});

test('an ancestor that refuses to be read is passed over on the way up', () => {
  const top = ancestor({ __tcfapiLocator: {} });
  const refusing = {
    get frames() {
      throw new Error('cross origin');
    },
    parent: top,
    top,
  };
  const page = browser({ parent: refusing, top });

  assert.equal(page.window.__tcfapi, undefined, 'the CMP above it is found');
});

test('a page inside a frame with no CMP above it gets the stub', () => {
  const top = ancestor();
  const page = browser({ parent: top });

  assert.equal(typeof page.window.__tcfapi, 'function');
  assert.equal(page.appended.length, 1, 'the locator frame is added');
});
