// Turn the test browser into a fresh visitor: clear every cookie and the storage of the
// given origins (the publisher origin and any CMP origin, so stored consent goes too).
//
// Usage: node reset.mjs --origin https://<publisher-host> [--origin https://<cmp-host> ...]
//                       [--cdp http://127.0.0.1:9222]
import { connect, list, parseArgs } from './cdp-lib.mjs';

const { options } = parseArgs(process.argv.slice(2));
const origins = list(options.origin);
if (origins.length === 0) throw new Error('at least one --origin is required');

const session = await connect({ cdp: options.cdp ?? 'http://127.0.0.1:9222' });
await session.send('Page.navigate', { url: 'about:blank' });
await session.send('Network.enable');
await session.send('Network.clearBrowserCookies');
for (const origin of origins) {
  await session.send('Storage.clearDataForOrigin', { origin, storageTypes: 'all' });
}
const { cookies } = await session.send('Network.getAllCookies');
console.log(JSON.stringify({ clearedOrigins: origins, remainingCookies: cookies.length }));
session.close();
