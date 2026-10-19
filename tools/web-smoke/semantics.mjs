// Driving a Flutter web page through its semantics tree.
//
// Flutter draws to a canvas, so there is no DOM of buttons and fields to click.
// Once the semantics tree is on, every control a person can see is an
// `flt-semantics` element with its text or label on it. These helpers find
// one by what it says and click it at its centre. Every function takes a
// device: `{ name, page }`, where `name` is only for messages.

/// Load [to] in the device's page and turn the semantics tree on.
export async function load(d, to) {
  await d.page.goto(to, { waitUntil: 'load' });
  await d.page.waitForSelector('flutter-view', { timeout: 120_000 });
  await d.page.waitForTimeout(5000);
  await accessible(d);
}

/// Select a relay for a fresh browser profile through the same Settings flow a
/// person uses. Local-only is the default; tests must opt into the relay they
/// exercise instead of relying on a build-time address.
export async function configureRelay(d, url) {
  await tap(d, /navigation menu/i);
  await tap(d, /Sync & devices/);
  await tap(d, /^Set up sync$/);
  await fill(d, 'Relay URL or relay link', url);
  await tap(d, /^Set up sync$/);
  await tap(d, /^Start with an empty collection$/);
  // Settings closes itself once the new profile is open. If it stays, the
  // switch has not finished, which is a failure and not something to tap past.
  await find(d, n => n.text.trim() === 'New note', { timeout: 30_000 });
}

/// Turn the semantics tree on. The placeholder is deliberately off-screen, so
/// the click is dispatched rather than performed. A tab can drop the tree again
/// when another one takes the foreground, so [find] asks for it again whenever
/// it finds none.
export async function accessible(d) {
  await d.page.evaluate(() =>
    document.querySelector('flt-semantics-placeholder')
      ?.dispatchEvent(new PointerEvent('click', { bubbles: true })));
  await d.page.waitForTimeout(1500);
}

/// The semantics nodes a person could see: what each says, the label of the
/// field it holds, whether it is ticked, and where its centre is.
// A heading, such as an app bar's title, is drawn as an `<h1>`–`<h6>` of its
// own rather than as `flt-semantics`, so it is asked for by name.
export const nodes = d => d.page.evaluate(() =>
  Array.from(document.querySelectorAll(
    'flt-semantics, flt-semantics-host :is(h1, h2, h3, h4, h5, h6)',
  )).map(e => {
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
export const texts = async d => (await nodes(d)).map(n => n.text.trim()).filter(Boolean);

/// Wait for a node to match, and return it. The tree is rebuilt between
/// frames, so a single look can land on it half-built.
export async function find(d, match, { timeout = 10_000 } = {}) {
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
      throw new Error(`${d.name}: nothing matched ${match.what ?? match}\n  on screen: ${(await texts(d)).join(' | ')}\n  page errors: ${(d.crashes ?? []).slice(0, 3).join(' | ')}\n  console errors: ${(d.errors ?? []).slice(0, 3).join(' | ')}`);
    }
    await d.page.waitForTimeout(250);
  }
}

export async function tap(d, re, options) {
  const n = await find(d, n => re.test(n.text.trim()), options);
  const target = d.page.locator('flt-semantics[role="button"]').filter({ hasText: re });
  if (await target.count()) await target.first().click({ force: true });
  else await d.page.mouse.click(n.x, n.y);
  await d.page.waitForTimeout(800);
}

export async function fill(d, label, value) {
  const n = await find(d, n => n.field === label);
  await d.page.mouse.click(n.x, n.y);
  await d.page.waitForTimeout(400);
  await d.page.keyboard.type(value);
  await d.page.waitForTimeout(400);
}

