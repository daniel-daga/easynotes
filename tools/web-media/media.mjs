// The browser's attachment store and transfer queue, in a real tab.
//
// `lib.rs` has the checks; this runs them from outside, then reads IndexedDB
// itself for plaintext, the way `web-smoke` reads what the app keeps. The relay
// is the one `just web-media` starts on the address the app defaults to, on a
// different origin from the page, so every media call is a CORS preflight with
// a bearer token and, for downloads, a `Range` header.
//
//     node tools/web-media/media.mjs <page url> <relay http base>
import { chromium } from 'playwright-core';

const url = process.argv[2] ?? 'http://127.0.0.1:8101/';
const relay = process.argv[3] ?? 'http://127.0.0.1:8080';
const executablePath = process.env.CHROME_EXECUTABLE;

const browser = await chromium.launch({
  ...(executablePath ? { executablePath } : { channel: 'chrome' }),
  args: ['--no-sandbox'],
});
let failed = false;
try {
  const page = await (await browser.newContext()).newPage();
  const crashes = [];
  page.on('pageerror', e => crashes.push(e.message));
  // Anything the page asks for that is not the page or the relay is a leak.
  const strays = [];
  page.on('request', r => {
    const origin = new URL(r.url()).origin;
    if (origin !== new URL(url).origin && origin !== new URL(relay).origin) strays.push(r.url());
  });

  await page.goto(url, { waitUntil: 'load' });
  await page.waitForFunction(() => window.media !== undefined, { timeout: 60_000 });

  const run = async (what, fn, ...args) => {
    try {
      console.log(`ok   ${await page.evaluate(fn, args)}`);
    } catch (e) {
      failed = true;
      console.log(`FAIL ${what}: ${e.message.split('\n')[0]}`);
    }
  };

  await run('store', ([db]) => window.media.store_checks(db), 'easynotes-media-checks');
  await run('relay', ([a, b, base]) => window.media.round_trip(a, b, base),
    'easynotes-media-a', 'easynotes-media-b', relay);

  // Every row of a database holding one import, searched for a stretch of
  // the photo and for its file name.
  await run('plaintext', async ([db]) => {
    await window.media.import_one(db);
    const sample = window.media.plaintext_sample();
    const name = new TextEncoder().encode('secret-name');
    const has = (bytes, needle) => {
      outer: for (let i = 0; i + needle.length <= bytes.length; i++) {
        for (let j = 0; j < needle.length; j++) if (bytes[i + j] !== needle[j]) continue outer;
        return true;
      }
      return false;
    };
    const database = await new Promise((resolve, reject) => {
      const r = indexedDB.open(db);
      r.onsuccess = () => resolve(r.result);
      r.onerror = () => reject(r.error);
    });
    const tx = database.transaction('media', 'readonly').objectStore('media');
    const [keys, values] = await Promise.all(['getAllKeys', 'getAll'].map(m =>
      new Promise((resolve, reject) => {
        const r = tx[m]();
        r.onsuccess = () => resolve(r.result);
        r.onerror = () => reject(r.error);
      })));
    database.close();
    if (keys.length < 5) throw new Error(`expected the import's rows, found ${keys.length}`);
    for (let i = 0; i < keys.length; i++) {
      if (keys[i].includes('secret-name')) throw new Error(`a file name in key ${keys[i]}`);
      if (has(values[i], sample) || has(values[i], name)) throw new Error(`plaintext in ${keys[i]}`);
    }
    return `plaintext: none in ${keys.length} rows`;
  }, 'easynotes-media-plaintext');

  if (crashes.length) {
    failed = true;
    console.log(`FAIL the page threw: ${crashes.join('; ')}`);
  }
  if (strays.length) {
    failed = true;
    console.log(`FAIL requests left the page and the relay: ${strays.join(', ')}`);
  }
} finally {
  await browser.close();
}
process.exit(failed ? 1 : 0);
