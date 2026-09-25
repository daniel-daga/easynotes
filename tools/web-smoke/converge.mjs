// Do two real UIs editing one list converge?
//
// Everything else that says two devices agree has a headless core on one side
// of it: `relay/tests` is two cores, `app/test/relay_sync_test.dart` is the
// app's API against a core, and `sync.mjs` is one tab against a relay. This is
// two tabs — two browser contexts, so two IndexedDBs and two accounts, as far
// apart as two people's phones — driven through the screens a person uses:
// one makes a list and shares it, the other opens the link and is let in, and
// then both edit it, online and then offline, and must end up with the same
// list.
//
// Flutter draws to a canvas, so everything here goes through the semantics
// tree: a node is found by the text or field label a person would see, and
// clicked at its centre. The helpers below are all there is to that.
//
//     node tools/web-smoke/converge.mjs <app url>
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

// A tab whose main thread is stuck answers no `evaluate`, and nothing in
// Playwright times one out, so the whole run is given a ceiling instead.
const started = Date.now();
const step = what => console.log(`--    ${what} (${Math.round((Date.now() - started) / 1000)}s)`);
setTimeout(() => {
  console.error(`\ngave up after ${Math.round((Date.now() - started) / 1000)}s: a tab stopped answering`);
  process.exit(2);
}, 6 * 60_000).unref();

/// One device: its own context, so its own storage and its own account. Tall
/// enough that nothing this test touches is below the fold.
async function device(name) {
  const context = await browser.newContext({ viewport: { width: 1000, height: 1200 } });
  // The share sheet, stood in for: what the app hands it is kept where the
  // test can read it, which is the link a person would have sent.
  await context.addInitScript(() => {
    navigator.canShare = () => true;
    navigator.share = async data => { window.__shared = data.text; };
  });
  const d = { name, context, crashes: [], errors: [] };
  // Every tab the device opens, including the one a link is opened in later.
  context.on('page', page => {
    page.on('pageerror', e => d.crashes.push(e.message));
    page.on('console', m => m.type() === 'error' && d.errors.push(m.text()));
  });
  d.page = await context.newPage();
  await load(d, url);
  return d;
}

async function load(d, to) {
  await d.page.goto(to, { waitUntil: 'load' });
  await d.page.waitForSelector('flutter-view', { timeout: 120_000 });
  await d.page.waitForTimeout(5000);
  await accessible(d);
}

/// Turn the semantics tree on. The placeholder is deliberately off-screen, so
/// the click is dispatched rather than performed. A tab can drop the tree again
/// when another one takes the foreground, so [find] asks for it again whenever
/// it finds none.
async function accessible(d) {
  await d.page.evaluate(() =>
    document.querySelector('flt-semantics-placeholder')
      ?.dispatchEvent(new PointerEvent('click', { bubbles: true })));
  await d.page.waitForTimeout(1500);
}

/// The semantics nodes a person could see: what each says, the label of the
/// field it holds, whether it is ticked, and where its centre is.
const nodes = d => d.page.evaluate(() =>
  Array.from(document.querySelectorAll('flt-semantics')).map(e => {
    const r = e.getBoundingClientRect();
    const input = e.querySelector('input, textarea');
    return {
      role: e.getAttribute('role') ?? '',
      // A container that merges what is inside it, like a card, says it in
      // its label rather than in text of its own.
      text: (e.querySelector('flt-semantics')
        ? Array.from(e.childNodes).filter(n => n.nodeType === 3).map(n => n.textContent).join('')
        : e.textContent) || (e.getAttribute('aria-label') ?? ''),
      field: input?.getAttribute('aria-label') ?? '',
      checked: e.getAttribute('aria-checked'),
      x: r.x + r.width / 2, y: r.y + r.height / 2, area: r.width * r.height,
    };
  }).filter(n => n.area > 0));

/// Every visible text, for reading what a screen says.
const texts = async d => (await nodes(d)).map(n => n.text.trim()).filter(Boolean);

/// Wait for a node to match, and return it. The tree is rebuilt between
/// frames, so a single look can land on it half-built.
async function find(d, match, { timeout = 10_000 } = {}) {
  const until = Date.now() + timeout;
  for (;;) {
    // The smallest match: a field's label is also on every node around it.
    const all = await nodes(d);
    if (all.length === 0) await accessible(d);
    const found = all.filter(match).sort((a, b) => a.area - b.area)[0];
    if (found) return found;
    if (Date.now() > until) {
      if (process.env.DEBUG_DIR) {
        await d.page.screenshot({ path: `${process.env.DEBUG_DIR}/${d.name}.png` });
        const html = await d.page.evaluate(() => document.querySelector('flt-semantics-host')?.outerHTML ?? '');
        (await import('node:fs')).writeFileSync(`${process.env.DEBUG_DIR}/${d.name}.html`, html);
      }
      throw new Error(`${d.name}: nothing matched ${match.what ?? match}\n  on screen: ${(await texts(d)).join(' | ')}`);
    }
    await d.page.waitForTimeout(250);
  }
}

async function tap(d, re, options) {
  const n = await find(d, n => re.test(n.text.trim()), options);
  await d.page.mouse.click(n.x, n.y);
  await d.page.waitForTimeout(800);
}

async function fill(d, label, value) {
  const n = await find(d, n => n.field === label);
  await d.page.mouse.click(n.x, n.y);
  await d.page.waitForTimeout(400);
  await d.page.keyboard.type(value);
  await d.page.waitForTimeout(400);
}

/// Add an item to the open list: a new row takes the focus, so it is typed
/// straight into.
async function addItem(d, text) {
  await tap(d, /^\+ Checklist item$/);
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

/// The list's card on the grid, once it shows every item in [want]. The card
/// is text, where the note screen's items are fields a canvas does not
/// expose, so this is where a device's copy of the list is read. It shows four
/// items and folds the rest into "+n more", so the list here never has more.
async function cardWith(d, want, timeout = 30_000) {
  if ((await texts(d)).includes('Back')) await tap(d, /^Back$/);
  return (await find(
    d,
    n => n.text.includes('milk') && want.every(item => n.text.includes(item)),
    { timeout },
  )).text;
}

/// Open the list again from its card.
async function reopen(d) {
  await tap(d, /milk/);
  await find(d, n => n.text.trim() === '+ Checklist item');
}

const a = await device('A');
const b = await device('B');

// --- A makes a list and shares it --------------------------------------------
await tap(a, /^New checklist$/);
await addItem(a, 'milk');
await tap(a, /^Share$/);
await setUpSharing(a, 'Ann');
await find(a, n => n.text.trim() === 'Share');
await tap(a, /^Send link$/);
await find(a, n => /^Nobody has opened it yet\.$/.test(n.text.trim()));
const message = await a.page.evaluate(() => window.__shared ?? '');
const link = message.slice(message.lastIndexOf('http'));
check(link.startsWith(url) && link.endsWith('#join#' + link.split('#').pop()), `A made a link: ${link.replace(/#join#.*/, '#join#…')}`);

// --- B opens it, and is let in without anyone asking -------------------------
// In a tab of its own, as a link opened from a message would be. The same tab
// would only change its fragment and never start the app again.
step('B closes its tab');
await b.page.close();
b.page = await b.context.newPage();
step('B opens the link in a new tab');
await load(b, link);
step('B sets up sharing');
await setUpSharing(b, 'Bea');
await tap(b, /^Join as Bea$/);
// A's share dialog is still open, and its poll is what lets Bea in: nobody on
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
await find(b, n => n.text.trim() === '+ Checklist item', { timeout: 30_000 });
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
check(a.crashes.length === 0, `A threw nothing: ${a.crashes.slice(0, 3).join(' | ')}`);
check(b.crashes.length === 0, `B threw nothing: ${b.crashes.slice(0, 3).join(' | ')}`);
// A panic in a synchronous bridge call is logged, not thrown: the one this
// test found (see `app/rust/src/lock.rs`) left no page error behind.
const errors = [...a.errors, ...b.errors];
check(errors.length === 0, `and logged no errors: ${errors.slice(0, 2).join(' | ').slice(0, 300)}`);
step('done');

await browser.close();
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
