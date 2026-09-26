// Exercise the GET arrival declared by the shipped Web Share Target manifest.
// This covers the real UI, bridge and IndexedDB; OS share-sheet registration
// still needs an installed PWA walkthrough.
import assert from 'node:assert/strict';
import { chromium } from 'playwright-core';
import { find, load, tap } from './semantics.mjs';

const url = process.argv[2] ?? 'http://127.0.0.1:8099/';
const browser = await chromium.launch({
  ...(process.env.CHROME_EXECUTABLE
    ? { executablePath: process.env.CHROME_EXECUTABLE } : { channel: 'chrome' }),
  args: ['--no-sandbox'],
});
const watchdog = setTimeout(() => {
  console.error('Shared-text smoke test timed out');
  process.exit(2);
}, 180_000).unref();

try {
  const context = await browser.newContext({
    viewport: { width: 1000, height: 1200 },
    permissions: ['clipboard-read', 'clipboard-write'],
  });
  const page = await context.newPage();
  const d = { name: 'shared-text', page };
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  // No relay is required. Return a deterministic unavailable response rather
  // than collecting browser connection-refused noise as a product error.
  await context.route('**/v1/**', route => route.fulfill({
    status: 503, headers: { 'Access-Control-Allow-Origin': '*' }, body: '',
  }));
  const manifestUrl = new URL('manifest.json', url).href;
  const response = await context.request.get(manifestUrl);
  assert(response.ok(), 'the shipped manifest loads');
  const target = (await response.json()).share_target;
  assert.equal(target.method, 'GET');
  for (const key of ['title', 'text', 'url']) assert(target.params[key]);

  async function ids() {
    return page.evaluate(async () => {
      const db = await new Promise((resolve, reject) => {
        const request = indexedDB.open('easynotes');
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      try {
        const keys = await new Promise((resolve, reject) => {
          const request = db.transaction('keys').objectStore('keys').getAllKeys();
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        });
        return keys.filter(k => k.startsWith('k/') && k !== 'k/AAAAAAAAAAAAAAAAAAAAAA').sort();
      } finally { db.close(); }
    });
  }

  async function exported(format) {
    await tap(d, /^More note actions$/);
    await tap(d, /^Send a copy$/);
    await find(d, n => n.text.trim() === `Share as ${format}`);
    await tap(d, /^Copy$/);
    // The Windows clipboard expands LF to CRLF; compare logical newlines.
    return (await page.evaluate(() => navigator.clipboard.readText())).replaceAll('\r\n', '\n');
  }

  const cases = [
    { title: 'A shared page', text: 'Café & tea\nBring cups.',
      url: 'https://example.test/read?a=1&b=2#section', format: 'text',
      expected: 'Café & tea\nBring cups.\nhttps://example.test/read?a=1&b=2#section' },
    { title: 'Shared groceries', text: '[ ] milk\n[x] bread\n[ ] eggs',
      format: 'checklist', expected: '[ ] milk\n[x] bread\n[ ] eggs' },
    { title: 'Mixed text', text: 'Read https://example.test/page\n[x] already done',
      url: 'https://example.test/page', format: 'text',
      expected: 'Read https://example.test/page\n[x] already done' },
  ];
  let previous = [];
  for (const test of cases) {
    const arrival = new URL(target.action, manifestUrl);
    for (const key of ['title', 'text', 'url']) {
      if (test[key]) arrival.searchParams.set(target.params[key], test[key]);
    }
    await load(d, arrival.href);
    await find(d, n => n.text.trim() === 'Back');
    assert.equal(new URL(page.url()).search, '', 'arrival query is consumed');
    assert.equal(await exported(test.format), test.expected, 'the imported content exports exactly');
    await tap(d, /^Back$/);
    await find(d, n => n.text.includes(test.title));
    const before = await ids();
    assert.equal(before.length, previous.length + 1, 'one share creates one note');
    assert(previous.every(id => before.includes(id)), 'previous shares remain');

    // Reload the cleaned address, never the original arrival URL.
    await load(d, page.url());
    assert.deepEqual(await ids(), before, 'reload keeps the same notes without importing again');
    await tap(d, new RegExp(test.title));
    assert.equal(await exported(test.format), test.expected, 'content and checked state survive reload');
    previous = before;
    console.log(`ok    ${test.title}: imported once, exact content retained after reload`);
  }
  assert.deepEqual(errors, [], 'no page exception or console error');
} finally {
  clearTimeout(watchdog);
  await browser.close();
}
