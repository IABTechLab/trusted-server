import { afterEach, expect, it, vi } from 'vitest';

import { importCreativeModule } from './helpers';

const currentScriptDescriptor = Object.getOwnPropertyDescriptor(document, 'currentScript');

afterEach(() => {
  vi.unstubAllGlobals();
  if (currentScriptDescriptor) {
    Object.defineProperty(document, 'currentScript', currentScriptDescriptor);
  } else {
    Reflect.deleteProperty(document, 'currentScript');
  }
  document.body.innerHTML = '';
});

it('leaves configured-origin images and iframes unchanged through setters and mutation processing', async () => {
  vi.resetModules();
  const fetchMock = vi.fn().mockRejectedValue(new Error('unexpected signing request'));
  vi.stubGlobal('fetch', fetchMock);
  const script = document.createElement('script');
  script.src = `${location.origin}/static/runtime.js`;
  script.setAttribute('data-ts-public-origin', 'https://ads.publisher.example:8443');
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
  await importCreativeModule({ renderGuard: true });

  const proxy =
    'https://ads.publisher.example:8443/first-party/proxy?tsurl=https%3A%2F%2Fresource.example%2Fasset&tstoken=signed';
  const image = new Image();
  const iframe = document.createElement('iframe');
  image.src = proxy;
  iframe.src = proxy;
  document.body.append(image, iframe);
  // innerHTML bypasses the src setters and setAttribute hooks. These resources
  // must pass through the guard's MutationObserver, including later changes.
  const container = document.createElement('div');
  container.innerHTML = `<img src="${proxy}"><iframe src="${proxy}"></iframe>`;
  document.body.appendChild(container);
  await new Promise((resolve) => setTimeout(resolve, 10));
  for (const element of document.querySelectorAll('img, iframe')) {
    expect(element.getAttribute('src')).toBe(proxy);
    element.setAttribute('src', `${proxy}&version=2`);
  }
  await new Promise((resolve) => setTimeout(resolve, 10));
  for (const element of document.querySelectorAll('img, iframe')) {
    expect(element.getAttribute('src')).toBe(`${proxy}&version=2`);
  }
  expect(fetchMock).not.toHaveBeenCalled();
});
