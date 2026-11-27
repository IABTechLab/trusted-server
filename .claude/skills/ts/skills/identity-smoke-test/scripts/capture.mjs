// Capture the document and Trusted Server (/_ts/, /first-party/, /auction) traffic of one
// page view, plus the identity cookies left in the browser afterwards.
//
// Usage:
//   node capture.mjs --host <publisher-host> [--cdp http://127.0.0.1:9222]
//                    [--cookie <partner-cookie-name> ...] [--json] <url|reload|watch> [settleMs]
//
// `watch` records whatever the page sends during settleMs without navigating, so you can
// act in the browser (for example save a CMP choice) while it listens.
//
// Cookie values are never printed: each is reduced to its length, except `ts-ec`, which
// keeps an 8-character prefix so continuity across page views can be compared.
import { connect, list, parseArgs, sleep } from './cdp-lib.mjs';

const { options, positionals } = parseArgs(process.argv.slice(2));
const host = options.host;
if (!host) throw new Error('--host <publisher-host> is required');
const cdp = options.cdp ?? 'http://127.0.0.1:9222';
const [target = 'reload', settleArg = '10000'] = positionals;
const settleMs = Number(settleArg);

const IDENTITY_COOKIES = new Set([
  'ts-ec', '__Host-ts-eids-mirror', 'ts-eids', '__Host-ts-tcf', '_ts_gpp_src',
  '__gpp', '__gpp_sid', 'euconsent-v2', 'usprivacy', 'us_privacy',
  '_pubcid', '_pubcid_cst', 'sharedId', '_sharedid', '_sharedid_cst',
  ...list(options.cookie),
]);
const isIdentityCookie = (name) => IDENTITY_COOKIES.has(name) || name.startsWith('ts-') || name.startsWith('__Host-ts');

const redactSetCookie = (line) => {
  const [pair, ...attributes] = line.split(';');
  const separator = pair.indexOf('=');
  const name = pair.slice(0, separator).trim();
  const value = pair.slice(separator + 1);
  const shown = name === 'ts-ec' && value ? `${value.slice(0, 8)}…(${value.length})` : `<${value.length}>`;
  return { name, line: `${name}=${shown};${attributes.join(';')}`, expires: /max-age=0\b|expires=thu, 01 jan 1970|expires=wed, 01 jan 2020/i.test(line) };
};
const redactBody = (body) =>
  body && body.replace(/("(?:gppString|tcString|euconsent|consentString)"\s*:\s*")([^"]*)"/g, (_, key, value) => `${key}<${value.length} chars>"`);

const isInteresting = (url, type) => {
  try {
    const parsed = new URL(url);
    return parsed.host === host && (type === 'Document' || /^\/(_ts|__ts|first-party|auction|integrations)\//.test(`${parsed.pathname}/`));
  } catch {
    return false;
  }
};

const session = await connect({ cdp, host });
const requests = new Map();
// CDP may deliver a request's ExtraInfo (which carries the real Cookie header) before
// requestWillBeSent, so keep it until the request it belongs to is known.
const earlyRequestExtraInfo = new Map();
const applySentCookies = (request, headers) => {
  const cookieHeader = Object.entries(headers).find(([key]) => key.toLowerCase() === 'cookie')?.[1] ?? '';
  const sent = cookieHeader.split('; ').filter(Boolean);
  request.sentCookies = sent.map((cookie) => cookie.split('=')[0]).filter(isIdentityCookie);
  request.sentEcPrefix = sent.find((cookie) => cookie.startsWith('ts-ec='))?.slice(6, 14) ?? null;
};
session.on((message) => {
  const params = message.params;
  switch (message.method) {
    case 'Network.requestWillBeSent':
      if (isInteresting(params.request.url, params.type)) {
        const request = {
          wallMs: Math.round(params.wallTime * 1000),
          method: params.request.method,
          url: params.request.url,
          type: params.type,
          body: redactBody(params.request.postData),
        };
        const early = earlyRequestExtraInfo.get(params.requestId);
        if (early) applySentCookies(request, early);
        requests.set(params.requestId, request);
      }
      earlyRequestExtraInfo.delete(params.requestId);
      break;
    case 'Network.requestWillBeSentExtraInfo': {
      const request = requests.get(params.requestId);
      if (request) applySentCookies(request, params.headers);
      else earlyRequestExtraInfo.set(params.requestId, params.headers);
      break;
    }
    case 'Network.responseReceived': {
      const request = requests.get(params.requestId);
      if (request) request.status = params.response.status;
      break;
    }
    case 'Network.responseReceivedExtraInfo': {
      const request = requests.get(params.requestId);
      if (!request) break;
      // Chrome folds repeated Set-Cookie headers into one newline-separated value.
      const raw = Object.entries(params.headers)
        .filter(([key]) => key.toLowerCase() === 'set-cookie')
        .map(([, value]) => value)
        .join('\n');
      request.setCookies = raw.split('\n').filter(Boolean).map(redactSetCookie).filter((cookie) => isIdentityCookie(cookie.name));
      request.blockedSetCookies = (params.blockedCookies ?? []).map(
        (cookie) => `${cookie.cookie?.name ?? cookie.cookieLine.split('=')[0]}:${cookie.blockedReasons.join('|')}`,
      );
      const header = (name) => Object.entries(params.headers).find(([key]) => key.toLowerCase() === name)?.[1];
      request.headers = {
        'x-ts-version': header('x-ts-version'),
        'x-geo-country': header('x-geo-country'),
        'x-geo-region': header('x-geo-region'),
        'x-geo-info-available': header('x-geo-info-available'),
        'cache-control': header('cache-control'),
      };
      break;
    }
    default:
  }
});

await session.send('Network.enable', { maxPostDataSize: 8192 });
await session.send('Page.enable');
let markLoaded;
const loaded = new Promise((resolve) => { markLoaded = resolve; });
session.on((message) => { if (message.method === 'Page.loadEventFired') markLoaded(); });
if (target === 'watch') markLoaded();
else if (target === 'reload') await session.send('Page.reload', {});
else await session.send('Page.navigate', { url: target });
await Promise.race([loaded, sleep(60000)]);
await sleep(settleMs);

const { cookies } = await session.send('Network.getCookies', { urls: [`https://${host}/`] });
const jar = cookies.filter((cookie) => isIdentityCookie(cookie.name)).map((cookie) => ({
  name: cookie.name,
  domain: cookie.domain,
  httpOnly: cookie.httpOnly,
  sameSite: cookie.sameSite ?? '',
  expiresInDays: cookie.expires > 0 ? Math.round((cookie.expires - Date.now() / 1000) / 86400) : 'session',
  value: cookie.name === 'ts-ec' ? `${cookie.value.slice(0, 8)}…(${cookie.value.length})` : `<${cookie.value.length}>`,
}));
const report = { page: session.url, target, requests: [...requests.values()], jar };
session.close();

if (options.json) {
  console.log(JSON.stringify(report, null, 1));
} else {
  const start = report.requests[0]?.wallMs ?? 0;
  for (const request of report.requests) {
    const path = new URL(request.url).pathname;
    const set = (request.setCookies ?? []).map((cookie) => `${cookie.name}${cookie.expires ? ' EXPIRED' : ''}`).join(', ');
    console.log(
      `+${String(request.wallMs - start).padStart(5)}ms ${request.method} ${path.slice(0, 48)} ${request.status ?? '-'}` +
        `${request.body ? ` body=${request.body}` : ''} sentEC=${request.sentEcPrefix ?? '-'}` +
        ` sent=[${(request.sentCookies ?? ['?']).join(', ')}] set=[${set}]` +
        `${request.blockedSetCookies?.length ? ` BLOCKED=[${request.blockedSetCookies.join(', ')}]` : ''}`,
    );
  }
  const document = report.requests.find((request) => request.type === 'Document');
  if (document?.headers) console.log('document headers:', JSON.stringify(document.headers));
  console.table(jar);
}
