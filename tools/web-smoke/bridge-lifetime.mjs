import { chromium } from 'playwright-core';
import { mkdirSync, createWriteStream } from 'node:fs';
import { createGzip } from 'node:zlib';
import { finished } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';

// Compile app/tool/bridge_lifetime.dart to app/build/web/bridge_lifetime.js
// first, then serve app/build/web with serve.py. See README.md.
const url = process.argv[2] ?? 'http://127.0.0.1:8099/';
const rounds = Number(process.env.LIFETIME_ROUNDS ?? 30);
const count = Number(process.env.LIFETIME_BATCH ?? 100);
const mode = process.env.LIFETIME_MODE ?? 'gc';
if (!Number.isInteger(rounds) || rounds < 1 || !Number.isInteger(count) || count < 1
    || !['gc', 'reopen', 'pending'].includes(mode)) throw new Error('Invalid lifetime test configuration');
const directory = fileURLToPath(new URL('../../target/bridge-lifetime/', import.meta.url));
mkdirSync(directory, { recursive: true });
const output = createWriteStream(`${directory}/${Date.now()}-${mode}.jsonl.gz`);
const trace = createGzip();
trace.pipe(output);
const timeout = setTimeout(() => { console.error('Lifetime test timed out'); process.exit(2); }, 10 * 60_000);
timeout.unref();

const browser = await chromium.launch({
  ...(process.env.CHROME_EXECUTABLE ? { executablePath: process.env.CHROME_EXECUTABLE } : { channel: 'chrome' }),
  args: ['--js-flags=--expose-gc'],
});
const context = await browser.newContext();
await context.addInitScript(({ captureStacks, workers }) => {
  Error.stackTraceLimit = 50;
  if (workers) Object.defineProperty(navigator, 'hardwareConcurrency', { value: workers });
  const Native = globalThis.FinalizationRegistry;
  const events = globalThis.lifetimeEvents = [];
  let serial = 0;
  globalThis.FinalizationRegistry = class {
    constructor(callback) {
      this.tokens = new WeakMap();
      this.registry = new Native(record => {
        events.push(['finalize', record.id, record.token, record.active]);
        if (!record.active) throw new Error(`Finalizer ran after detach: ${record.id}`);
        record.active = false;
        try { callback(record.token); }
        catch (error) {
          console.error('LIFETIME FINALIZER', JSON.stringify(record), String(error), error.stack);
          throw error;
        }
      });
    }
    register(target, token, detach) {
      const record = { id: serial++, token, active: true,
        ...(captureStacks ? { stack: new Error().stack } : {}) };
      events.push(['register', record.id, token]);
      if (detach) {
        const records = this.tokens.get(detach) ?? [];
        records.push(record);
        this.tokens.set(detach, records);
      }
      this.registry.register(target, record, detach);
    }
    unregister(detach) {
      for (const record of this.tokens.get(detach) ?? []) {
        record.active = false;
        events.push(['detach', record.id, record.token]);
      }
      this.tokens.delete(detach);
      return this.registry.unregister(detach);
    }
  };
}, { captureStacks: !!process.env.LIFETIME_JS_STACKS, workers: Number(process.env.LIFETIME_WORKERS) || null });
await context.route('**/bridge_lifetime.html', route => route.fulfill({
  contentType: 'text/html',
  headers: { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp' },
  body: '<!doctype html><link rel="icon" href="data:,"><script src="bridge_lifetime.js" defer></script>',
}));
let failed = false;
let pageNumber = 0;
context.on('page', page => {
  const number = pageNumber++;
  page.on('console', message => {
    trace.write(JSON.stringify({ page: number, type: message.type(), message: message.text() }) + '\n');
    if (message.type() === 'error') { failed = true; console.error(message.text()); }
  });
  page.on('pageerror', error => {
    failed = true;
    trace.write(JSON.stringify({ page: number, type: 'pageerror', message: error.stack }) + '\n');
    console.error(error);
  });
});
let page;
try {
  for (let batch = 0; batch < rounds && !failed; batch++) {
    if (!page || mode !== 'gc') {
      if (page) await page.close();
      page = await context.newPage();
      await page.goto(new URL('bridge_lifetime.html', url).href);
      await page.waitForFunction(() => globalThis.lifetimeReady, null, { timeout: 120000 });
    }
    await page.evaluate(count => lifetimeBatch(count), count);
    if (mode !== 'pending') {
      await page.evaluate(() => globalThis.gc());
      await page.waitForFunction(() => lifetimeEvents.some(e => e[0] === 'finalize'));
    }
    const events = await page.evaluate(() => lifetimeEvents.splice(0));
    trace.write(JSON.stringify({ page: pageNumber - 1, type: 'finalizers', events }) + '\n');
    console.log(`batch ${batch}:`, {
      finalized: events.filter(e => e[0] === 'finalize').length,
      registered: events.filter(e => e[0] === 'register').length,
    });
  }
} finally {
  await browser.close();
  trace.end();
  await finished(output);
  clearTimeout(timeout);
}
if (failed) process.exitCode = 1;
