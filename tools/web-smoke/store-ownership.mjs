// Compile app/tool/browser_store_ownership.dart to app/build/store-ownership.js.
// Exercises the actual Dart adapter in separate pages of one browser profile.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { chromium } from 'playwright-core';

const script = readFileSync(new URL('../../app/build/store-ownership.js', import.meta.url));
const server = createServer((req, res) => {
  res.setHeader('Content-Type', req.url === '/store.js' ? 'application/javascript' : 'text/html');
  res.end(req.url === '/store.js' ? script : '<!doctype html><script src="/store.js"></script>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({
    channel: process.env.CHROME_EXECUTABLE ? undefined : 'chrome',
    executablePath: process.env.CHROME_EXECUTABLE,
    headless: true,
  });
  const context = await browser.newContext();
  const url = `http://127.0.0.1:${server.address().port}/`;
  const page = async () => {
    const result = await context.newPage();
    await result.goto(url);
    await result.waitForFunction(() => typeof storeOpen === 'function');
    return result;
  };
  const first = await page();
  const second = await page();
  await first.evaluate(() => storeOpen('ownership'));
  const waitForOwner = async p => {
    await p.evaluate(() => {
      window.acquired = false;
      window.opening = storeOpen('ownership').then(() => { window.acquired = true; });
    });
    await p.waitForFunction(async () => (await navigator.locks.query()).pending.length === 1);
    assert.equal(await p.evaluate(() => window.acquired), false);
  };
  await waitForOwner(second);
  await first.evaluate(() => storeWrite(1));
  await first.evaluate(() => storeClose());
  await second.waitForFunction(() => window.acquired);
  assert.deepEqual(await second.evaluate(() => storeRead()), [1]);
  await second.evaluate(() => storeWrite(2));
  // Closing an already closed handle must not release the successor's lock.
  await first.evaluate(() => storeClose());
  const third = await page();
  await waitForOwner(third);
  // Abrupt tab closure must release ownership without any Dart cleanup.
  await second.close();
  await third.waitForFunction(() => window.acquired);
  assert.deepEqual(await third.evaluate(() => storeRead()), [1, 2]);
  await third.evaluate(() => storeClose());
  console.log('PASS: tabs wait, committed edits survive handoff, and tab closure releases ownership.');
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
