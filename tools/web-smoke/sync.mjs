// Does a tab actually reach a relay?
//
// The web transport (`easynotes_core::sync::client_web`) is the one part of
// syncing that no `cargo test` can run: it is the browser's own WebSocket and
// `fetch`, and neither exists outside a browser. What `cargo test` does cover is
// everything above and below it — `sync::wire` is the protocol both transports
// speak, and `relay/tests` drives the relay with real cores — so what is left
// for this is the join between them.
//
// It asserts on the frames rather than on the UI, which is both simpler and
// stronger. The relay echoes an append back to its sender as an `update`; that
// echo is how the engine knows the relay has an envelope (ADR 0004). So an
// `append` leaving the tab and an `update` carrying the same envelope arriving
// back is a full round trip through a real relay, and nothing else produces it.
//
//     node tools/web-smoke/sync.mjs <app url>
import { chromium } from 'playwright-core';

const url = process.argv[2] ?? 'http://127.0.0.1:8099/';
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

const sockets = [];
const sent = [];
const received = [];
page.on('websocket', ws => {
  sockets.push(ws.url());
  ws.on('framesent', f => sent.push(String(f.payload)));
  ws.on('framereceived', f => received.push(String(f.payload)));
});
const crashes = [];
page.on('pageerror', e => crashes.push(e.message));

await page.goto(url, { waitUntil: 'load' });
await page.waitForSelector('flutter-view', { timeout: 120_000 });
await page.waitForTimeout(6000);
// The canvas has no DOM until the semantics tree is turned on, and the
// placeholder is deliberately off-screen, so the click is dispatched rather
// than performed.
await page.evaluate(() =>
  document.querySelector('flt-semantics-placeholder')
    ?.dispatchEvent(new PointerEvent('click', { bubbles: true })));
await page.waitForTimeout(2500);

// The floating button, then the title field and the item field. Positions come
// from the semantics tree so a change in layout moves them with it.
const box = async (nth) => page.evaluate((n) => {
  const buttons = Array.from(document.querySelectorAll('flt-semantics[role="button"]'));
  const r = buttons.at(n).getBoundingClientRect();
  return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
}, nth);

const fab = await box(-1);
await page.mouse.click(fab.x, fab.y);
await page.waitForTimeout(3000);
await page.mouse.click(600, 28);
await page.waitForTimeout(800);
await page.keyboard.type('Groceries');
await page.waitForTimeout(800);
await page.mouse.click(600, 688);
await page.waitForTimeout(800);
await page.keyboard.type('asparagus');
await page.keyboard.press('Enter');
// The handshake is two HTTP calls and then a socket, and the relay may be cold.
await page.waitForTimeout(6000);

// One socket for the device, not one per note (ADR 0017).
const device = sockets.filter(u => u.includes('/v1/sync'));
check(device.length === 1, `the tab opened one socket for the device: ${sockets.join(' ') || '(none)'}`);
check(
  !sockets.some(u => u.includes('/v1/notes/')),
  'and no socket per note',
);
check(
  device.length > 0 && /[?&]token=/.test(device[0]),
  'it carried a token, so the two HTTP calls of the handshake went through CORS',
);
check(
  sent.some(f => f.includes('"type":"subscribe"')),
  'the tab subscribed',
);

const appended = sent.filter(f => f.includes('"type":"append"'));
check(appended.length > 0, `the tab published its edits (${appended.length} appends)`);

// The echo: the relay fans an append back to its sender, and that is what the
// engine treats as the acknowledgement.
const envelopeOf = f => { try { return JSON.parse(f).envelope; } catch { return null; } };
const out = new Set(appended.map(envelopeOf).filter(Boolean));
const back = received.filter(f => f.includes('"type":"update"')).map(envelopeOf);
check(
  back.some(e => out.has(e)),
  'the relay echoed one of them back, which is a full round trip through it',
);
check(
  received.some(f => f.includes('"type":"caught"')),
  'the relay said the tab was caught up',
);
check(crashes.length === 0, `the page threw nothing: ${crashes.slice(0, 3).join(' | ')}`);

await browser.close();
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  console.error(`sent:     ${sent.slice(0, 6).join('\n          ')}`);
  console.error(`received: ${received.slice(0, 6).join('\n          ')}`);
  process.exit(1);
}
console.log('\na tab reaches the relay, publishes, and is acknowledged');
