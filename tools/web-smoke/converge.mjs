// Do two real UIs editing one list converge?
//
// Everything else that says two devices agree has a headless core on one side
// of it: `relay/tests` is two cores, `app/test/relay_sync_test.dart` is the
// app's API against a core, and `sync.mjs` is one tab against a relay. This is
// two tabs — two browser contexts, so two IndexedDBs and two accounts, as far
// apart as two people's phones — driven through the screens a person uses:
// one makes a list and shares it, the other opens the link and is let in, and
// then both edit it, online and then offline, and must end up with the same
// list. Then one turns it into a text note and the other turns it back, and
// each switch has to reach the other tab.
//
// Flutter draws to a canvas, so everything here goes through the semantics
// tree: a node is found by the text or field label a person would see, and
// clicked at its centre. `semantics.mjs` is all there is to that.
//
//     node tools/web-smoke/converge.mjs <app url>
import { chromium } from 'playwright-core';
import { configureRelay, fill, find, load, nodes, tap, texts } from './semantics.mjs';

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

// A tab whose main thread is stuck answers no `evaluate`, and nothing in
// Playwright times one out, so the whole run is given a ceiling instead.
const started = Date.now();
const elapsed = () => Math.round((Date.now() - started) / 1000);
let currentStep = 'browser startup';
const step = what => {
  currentStep = what;
  console.log(`--    ${what} (${elapsed()}s)`);
};
const devices = [];
setTimeout(async () => {
  console.error(`\ngave up after ${elapsed()}s: a tab stopped answering`);
  await Promise.race([diagnose(), new Promise(r => setTimeout(r, 30_000))]);
  process.exit(2);
}, 6 * 60_000).unref();

/// Where each tab is stuck: the page thread's stack and every worker's,
/// paused through the DevTools protocol, which answers even when the page
/// does not. And the last heap samples, which tell a spin from a tab that ran
/// out of memory.
async function diagnose() {
  const frames = paused => paused.callFrames.slice(0, 30).map(f =>
    `      ${f.functionName || '<anonymous>'}  ${f.url.split('/').pop()}:${f.location.lineNumber + 1}`,
  ).join('\n');
  for (const d of devices) {
    console.error(`\n${d.name}: heap ${d.heap.slice(-4).join(', ') || 'never sampled'}`);
    try {
      const cdp = await d.context.newCDPSession(d.page);
      await cdp.send('Debugger.enable');
      const paused = new Promise(r => cdp.once('Debugger.paused', r));
      await cdp.send('Debugger.pause');
      console.error(`  page thread:\n${frames(await paused)}`);
    } catch (e) {
      console.error(`  page thread: ${e.message}`);
    }
  }
  // Workers are not pages, so they are reached through the browser's own
  // session and messages relayed to each.
  const cdp = await browser.newBrowserCDPSession();
  const { targetInfos } = await cdp.send('Target.getTargets');
  for (const worker of targetInfos.filter(t => /worker/.test(t.type))) {
    try {
      const { sessionId } = await cdp.send('Target.attachToTarget', { targetId: worker.targetId, flatten: false });
      const replies = new Map();
      let paused;
      cdp.on('Target.receivedMessageFromTarget', m => {
        if (m.sessionId !== sessionId) return;
        const msg = JSON.parse(m.message);
        if (msg.method === 'Debugger.paused') paused?.(msg.params);
        else replies.get(msg.id)?.(msg);
      });
      let id = 0;
      const send = (method, params = {}) => new Promise(r => {
        replies.set(++id, r);
        cdp.send('Target.sendMessageToTarget', { sessionId, message: JSON.stringify({ id, method, params }) });
      });
      const stopped = new Promise(r => { paused = r; });
      await send('Debugger.enable');
      await send('Debugger.pause');
      console.error(`  ${worker.type} ${worker.url.split('/').pop()}:\n${frames(await stopped)}`);
    } catch (e) {
      console.error(`  ${worker.type}: ${e.message}`);
    }
  }
}

/// One device: its own context, so its own storage and its own account. Tall
/// enough that nothing this test touches is below the fold.
async function device(name) {
  const context = await browser.newContext({ viewport: { width: 1000, height: 1200 } });
  // The share sheet, stood in for: what the app hands it is kept where the
  // test can read it, which is the link a person would have sent.
  await context.addInitScript(() => {
    // Chrome keeps ten frames, and a Rust panic's hook takes most of them.
    Error.stackTraceLimit = 200;
    navigator.canShare = () => true;
    navigator.share = async data => { window.__shared = data.text; };
  });
  const d = { name, context, crashes: [], errors: [], heap: [] };
  devices.push(d);
  // A tab the test has deliberately taken offline will still try to reach the
  // relay, and the browser logs the failed request as a console error. That is
  // the condition under test, not an app error, so it is not counted.
  const offlineNoise = /net::ERR_(INTERNET_DISCONNECTED|NETWORK_CHANGED)/;
  // Every tab the device opens, including the one a link is opened in later.
  context.on('page', page => {
    page.on('pageerror', e => d.crashes.push(e.message));
    page.on('console', m => m.type() === 'error' &&
      !offlineNoise.test(m.text()) &&
      d.errors.push(`[${currentStep}] ${m.text()}`));
    page.on('crash', async () => {
      console.error(`\n${d.name}'s tab crashed at ${elapsed()}s`);
      await diagnose();
    });
  });
  d.page = await context.newPage();
  // A sample stops arriving while the page thread is stuck, which is itself
  // worth knowing; the protocol's heap query runs on that thread.
  // It follows whichever tab is the device's now.
  let sampled, heap;
  setInterval(async () => {
    try {
      if (sampled !== d.page) [sampled, heap] = [d.page, await context.newCDPSession(d.page)];
      const h = await heap.send('Runtime.getHeapUsage');
      d.heap.push(`${elapsed()}s ${Math.round(h.usedSize / 2 ** 20)} MiB`);
    } catch {
      // A closed or crashed tab: the next sample tries the current one.
    }
  }, 5_000).unref();
  await load(d, url);
  return d;
}

/// Add an item to the open list: a new row takes the focus, so it is typed
/// straight into.
async function addItem(d, text) {
  await tap(d, /^Checklist item$/);
  // Typed into the editing element Flutter focuses for the new row, once it
  // has: before then the first keystrokes go nowhere, and on a slow build
  // "bread" arrives as "read".
  await d.page.waitForFunction(() => /^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName ?? ''));
  await d.page.keyboard.type(text);
  await d.page.waitForTimeout(600);
}

/// The first time a device shares or joins: make an account, pass the check
/// that the twelve words were written down, and give a name.
async function setUpSharing(d, name) {
  await tap(d, /^Set up$/);
  await find(d, n => /^1\. \S+$/.test(n.text.trim()));
  const words = {};
  for (const t of await texts(d)) {
    const m = /^(\d+)\. (\S+)$/.exec(t);
    if (m) words[m[1]] = m[2];
  }
  check(Object.keys(words).length === 12, `${d.name} was shown twelve words`);
  await tap(d, /^I have written them down$/);
  await find(d, n => /^Word \d+$/.test(n.field));
  const asked = [...new Set((await nodes(d)).map(n => n.field).filter(f => /^Word \d+$/.test(f)))];
  for (const label of asked) await fill(d, label, words[label.split(' ')[1]]);
  await tap(d, /^Check$/);
  await fill(d, 'Name', name);
  await tap(d, /^Continue$/);
}

/// The list's card on the grid, once it shows every item in [want] and none
/// of [not]. The card is text, where the note screen's items are fields a
/// canvas does not expose, so this is where a device's copy of the list is
/// read. It shows four items and folds the rest into "+n more", so the list
/// here never has more.
async function cardWith(d, want, timeout = 30_000, not = []) {
  if ((await texts(d)).includes('Back')) await tap(d, /^Back$/);
  return (await find(
    d,
    n => n.text.includes('milk')
      && want.every(item => n.text.includes(item))
      && !not.some(item => n.text.includes(item)),
    { timeout },
  )).text;
}

/// Open the list again from its card.
async function reopen(d) {
  await tap(d, /milk/);
  await find(d, n => n.text.trim() === 'Checklist item');
}

/// Open the note from its card, whichever kind it is now, and flip it with
/// the button a person would use: "Hide checkboxes" on a checklist, "Show
/// checkboxes" on text. Waits for the button to say the other thing.
async function switchKind(d, to) {
  const [from, then] = to === 'text'
    ? ['Hide checkboxes', 'Show checkboxes']
    : ['Show checkboxes', 'Hide checkboxes'];
  await tap(d, /milk/);
  await tap(d, new RegExp(`^${from}$`));
  await find(d, n => n.text.trim() === then);
}

const a = await device('A');
const b = await device('B');

// Each browser context is a fresh device profile and starts local-only.
step('both choose the test relay');
await configureRelay(a, 'http://127.0.0.1:8080');
await configureRelay(b, 'http://127.0.0.1:8080');

// --- A makes a list and shares it --------------------------------------------
await tap(a, /^Create note/);
await tap(a, /^List(?:\n|$)/);
await addItem(a, 'milk');
await tap(a, /^Share$/);
await setUpSharing(a, 'Ann');
await find(a, n => n.text.trim() === 'Collaborators');
await tap(a, /^New invite link$/);
// The status cell says when the link expires on a line of its own.
await find(a, n => /^Not opened yet(\n|$)/.test(n.text.trim()));
const message = await a.page.evaluate(() => window.__shared ?? '');
const link = message.slice(message.lastIndexOf('http'));
check(link.startsWith(url) && link.endsWith('#join#' + link.split('#').pop()), `A made a link: ${link.replace(/#join#.*/, '#join#…')}`);

// --- B opens it, and is let in without anyone asking -------------------------
// In a tab of its own, as a link opened from a message would be. The same tab
// would only change its fragment and never start the app again.
step('B closes its tab');
await b.page.close();
// Let unload callbacks finish before this profile opens the same database again.
await new Promise(resolve => setTimeout(resolve, 1000));
b.page = await b.context.newPage();
step('B opens the link in a new tab');
await load(b, link);
step('B sets up sharing');
await setUpSharing(b, 'Bea');
await tap(b, /^Join as Bea$/);
// A's collaborators screen is still open, and its poll is what lets Bea in: nobody on
// A taps anything.
step('A lets Bea in');
check(
  !!(await find(a, n => n.text.includes('Bea joined.'), { timeout: 30_000 })),
  'A says Bea joined, without being asked',
);
// On the note's people, beside the owner, with a way to remove her.
check(
  !!(await find(a, n => /(^|\n)Bea$/.test(n.text.trim()), { timeout: 20_000 })),
  'A lists Bea among the people on the list',
);
await tap(a, /^Done$/);
step('B waits to be let in');
// B's waiting dialog polls, and opens the note when it is let in.
// It used to stop at "Opening the note" with the note already on the device:
// see `app/rust/src/lock.rs`.
await find(b, n => n.text.trim() === 'Checklist item', { timeout: 30_000 });
check(true, 'B was let in and the list opened for her');

// --- Both edit it, online ---------------------------------------------------
step('both edit online');
await addItem(b, 'bread');
const online = [await cardWith(a, ['bread']), await cardWith(b, ['bread'])];
check(
  online[0] === online[1],
  `online, both show the same list: ${JSON.stringify(online[0])}`,
);

// --- Both edit it, offline, and meet again ------------------------------------
step('both edit offline');
await Promise.all([reopen(a), reopen(b)]);
await a.context.setOffline(true);
await b.context.setOffline(true);
await addItem(a, 'apples');
await addItem(b, 'butter');
// Tick the first item, milk, which B did not add.
const box = await find(b, n => n.role === 'checkbox');
await b.page.mouse.click(box.x, box.y);
await b.page.waitForTimeout(800);
const apart = [await cardWith(a, ['apples']), await cardWith(b, ['butter'])];
await a.page.waitForTimeout(5000);
const stillApart = [await texts(a), await texts(b)];
check(
  !stillApart[0].some(t => t.includes('butter')) && !stillApart[1].some(t => t.includes('apples')),
  `offline, neither sees the other's edit: ${JSON.stringify(apart)}`,
);
step('both back online');
await a.context.setOffline(false);
await b.context.setOffline(false);
const met = [
  await cardWith(a, ['apples', 'butter', '1 of 4 done'], 90_000),
  await cardWith(b, ['apples', 'butter', '1 of 4 done'], 90_000),
];
check(
  met[0] === met[1],
  `back online, both show the same list: ${JSON.stringify(met[0])}`,
);
// --- One switches it to text, the other back to a list -----------------------
// The switch is one edit on the note's own document, so it is shared: the
// other person's card follows. Ticks do not survive the trip to text, as in
// Keep, so the list that comes back has four items and none of them done.
step('A switches the list to text');
await switchKind(a, 'text');
const asText = [
  await cardWith(a, ['apples', 'butter'], 30_000, ['done']),
  await cardWith(b, ['apples', 'butter'], 60_000, ['done']),
];
check(
  asText[0] === asText[1] && !asText[0].includes('•'),
  `A's switch to text reached B, with no bullets and no tally: ${JSON.stringify(asText[0])}`,
);
step('B switches it back to a checklist');
await switchKind(b, 'checklist');
const asList = [
  await cardWith(b, ['• apples', '• butter', '0 of 4 done'], 30_000),
  await cardWith(a, ['• apples', '• butter', '0 of 4 done'], 60_000),
];
check(
  asList[0] === asList[1],
  `B's switch back reached A, every line an item again: ${JSON.stringify(asList[0])}`,
);

check(a.crashes.length === 0, `A threw nothing: ${a.crashes.slice(0, 3).join(' | ')}`);
check(b.crashes.length === 0, `B threw nothing: ${b.crashes.slice(0, 3).join(' | ')}`);
// A panic in a synchronous bridge call is logged, not thrown: the one this
// test found (see `app/rust/src/lock.rs`) left no page error behind.
console.log(`A errors: ${a.errors.length}; B errors: ${b.errors.length}`);
const errors = [...a.errors, ...b.errors];
check(errors.length === 0, `and logged no errors: ${errors.slice(0, 2).join(' | ').slice(0, 300)}`);
for (const error of errors) console.error(`\nlogged:\n${error.slice(0, 6000)}`);
step('done');

await browser.close();
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
