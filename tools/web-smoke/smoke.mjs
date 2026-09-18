// Does the web build actually run in a browser, and keep what it is given?
//
// The only automated cover for the half of the web store that no `cargo test`
// or `flutter test` can reach. `easynotes_core::store::web` decides what is
// written and `core/tests` holds it to that; `app/test/rust_bridge_test.dart`
// drives a tab being closed and reopened over the real bridge. Neither has a
// browser in it, so neither can say whether IndexedDB was opened, whether the
// batches were applied, or whether a reloaded tab reads them back.
//
// ADR 0002 is why this exists at all: the two failures the web build has
// produced so far — a thread panic and a memory-cloning error — both compiled
// cleanly and only appeared when a browser ran the code.
//
// It asserts on the *database* and not on the UI. Flutter draws to a canvas, so
// there is nothing to read without turning the semantics tree on, and on a
// network that cannot reach fonts.gstatic.com the app renders its layout with
// no visible text at all — which is a real deferral (ADR 0002) and not
// something a storage test should be hostage to.
//
//     node tools/web-smoke/smoke.mjs <url>
import { chromium } from 'playwright-core';

const url = process.argv[2] ?? 'http://127.0.0.1:8099/';
// `playwright-core` brings no browser of its own, on purpose: a cloud session
// and a CI runner both already have one, and downloading a second is a hundred
// megabytes to run six assertions. CHROME_EXECUTABLE names it — the same
// variable Flutter uses for the same reason — and failing that, whatever
// Chrome the machine has installed.
const executablePath = process.env.CHROME_EXECUTABLE;

const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${what}`);
  if (!ok) failures.push(what);
};

const browser = await chromium.launch({
  ...(executablePath ? { executablePath } : { channel: 'chrome' }),
  args: ['--no-sandbox'],
});
const page = await (await browser.newContext()).newPage();
const crashes = [];
page.on('pageerror', e => crashes.push(e.message));

/// Everything in the app's IndexedDB, as keys to byte arrays.
const database = () => page.evaluate(async () => {
  const db = await new Promise((resolve, reject) => {
    const request = indexedDB.open('easynotes');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const out = {};
  for (const name of Array.from(db.objectStoreNames)) {
    const keys = await new Promise(r => {
      const q = db.transaction(name).objectStore(name).getAllKeys();
      q.onsuccess = () => r(q.result);
    });
    const values = await new Promise(r => {
      const q = db.transaction(name).objectStore(name).getAll();
      q.onsuccess = () => r(q.result);
    });
    out[name] = Object.fromEntries(keys.map((k, i) => [k, Array.from(values[i])]));
  }
  db.close();
  return out;
});

/// Load the app and wait for it to have done its first frame's worth of work.
async function open() {
  await page.goto(url, { waitUntil: 'load' });
  await page.waitForSelector('flutter-view', { timeout: 120_000 });
  // The app opens its database before the first frame, so the wait is for the
  // writes that opening a device makes rather than for anything drawn.
  await page.waitForFunction(
    () => window.indexedDB.databases().then(d => d.some(x => x.name === 'easynotes')),
    { timeout: 60_000 },
  );
  await page.waitForTimeout(3000);
}

await open();
check(await page.evaluate(() => crossOriginIsolated), 'the page is cross-origin isolated');

const first = await database();
check(
  JSON.stringify(first.notes?.schema) === JSON.stringify([1]),
  'the database has the schema row this build writes',
);
check(
  (first.keys?.['device-seed'] ?? []).length > 64,
  'the device identity is in the keyring, and is sealed rather than the raw 64 bytes',
);
// The account document is the first thing every device writes, under the
// reserved all-zero ID.
const account = 'AAAAAAAAAAAAAAAAAAAAAA';
check(`k/${account}` in (first.keys ?? {}), "the account document's key is kept");

// Nothing this app writes is a readable string. The envelopes are sealed under
// a note key and the metadata under the master key, so the only text a row
// should contain is the key name.
const values = Object.values(first).flatMap(rows => Object.values(rows));
check(
  !values.some(v => /[ -~]{12,}/.test(String.fromCharCode(...v))),
  'no row holds a readable run of text',
);

await open();
const second = await database();
check(
  JSON.stringify(second.keys?.['device-seed']) === JSON.stringify(first.keys?.['device-seed']),
  'a reloaded tab keeps the identity the first one minted',
);
check(
  Object.keys(second.notes ?? {}).length >= Object.keys(first.notes ?? {}).length,
  'a reloaded tab reads the rows the first one wrote rather than starting over',
);
check(crashes.length === 0, `the page threw nothing: ${crashes.slice(0, 3).join(' | ')}`);

await browser.close();
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
console.log('\nthe web build runs in a browser and keeps what it is given');
