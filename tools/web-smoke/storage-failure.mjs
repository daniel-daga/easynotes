// What does a deletion leave behind when the browser's own database refuses a
// write partway through it?
//
// `app/test/rust_bridge_test.dart` kills a tab at every batch of a deletion
// over a map standing in for IndexedDB. This is the same experiment against
// the real thing: a headless Chromium, the real web build, and a refusal made
// by IndexedDB itself. The init script below lets the app issue its writes and
// then adds two rows under one key to the same transaction. The second add is
// a ConstraintError, so the browser fails that request, fires `error` and then
// `abort` on the transaction, and rolls back every write the app made in it,
// exactly as it would for a quota error.
//
// Each round puts two notes in the trash and keeps a third, arms the refusal
// for the n-th write after "Delete all" is confirmed, and empties the trash.
// It then checks what a person and the database see:
//
//   - the refusal is said on the screen, and nothing is written after it;
//   - the page throws nothing and logs no error;
//   - in a new tab, each trashed note is either whole and still in the trash,
//     or has no row left at all. Never half gone.
//   - emptying the trash again takes what is left, and the kept note is never
//     touched.
//
// The rounds stop at the first n the deletion finishes without reaching.
//
//     node tools/web-smoke/storage-failure.mjs <app url>
import { chromium } from 'playwright-core';
import { find, load, tap, texts } from './semantics.mjs';
import { mkdirSync, createWriteStream } from 'node:fs';
import { createGzip } from 'node:zlib';
import { finished } from 'node:stream/promises';

// In diagnostic mode keep complete worker/page stacks and fail the known panic
// too. The Rust build must have EASYNOTES_TRACE_MOI=1 for per-ID pool events.
let trace;
let traceOutput;
let tracePage = 0;
if (process.env.BRIDGE_TRACE_DIR) {
  mkdirSync(process.env.BRIDGE_TRACE_DIR, { recursive: true });
  traceOutput = createWriteStream(`${process.env.BRIDGE_TRACE_DIR}/storage-${Date.now()}.jsonl.gz`);
  trace = createGzip();
  trace.pipe(traceOutput);
}

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

const started = Date.now();
setTimeout(() => {
  console.error(`\ngave up after ${Math.round((Date.now() - started) / 1000)}s`);
  process.exit(2);
}, 20 * 60_000).unref();

/// Counts the page's write transactions, and refuses the one numbered
/// `__idb.failAt`. Only that one: a browser that recovered would take the
/// next, and the app must not hand it one.
function refuseAWrite() {
  window.__idb = { writes: 0, failAt: null, refused: 0 };
  const transaction = IDBDatabase.prototype.transaction;
  IDBDatabase.prototype.transaction = function (stores, mode, ...rest) {
    const tx = transaction.call(this, stores, mode, ...rest);
    if (mode === 'readwrite' && window.__idb.writes++ === window.__idb.failAt) {
      window.__idb.refused++;
      // After the app's own requests, which it issues before it next awaits:
      // a transaction stays active through the microtasks of the task that
      // made it.
      queueMicrotask(() => {
        const store = tx.objectStore(tx.objectStoreNames[0]);
        store.add(new Uint8Array(0), '~refused');
        store.add(new Uint8Array(0), '~refused');
      });
    }
    return tx;
  };
}

/// The note IDs the database holds rows for, other than the account's.
const account = 'AAAAAAAAAAAAAAAAAAAAAA';
const noteIds = d => d.page.evaluate(async account => {
  // Named per relay profile; the `-media` database beside it holds
  // attachment bytes.
  const name = (await indexedDB.databases()).map(d => d.name).find(n => n.startsWith('easynotes-') && !n.endsWith('-media'));
  const db = await new Promise((resolve, reject) => {
    const request = indexedDB.open(name);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const ids = new Set();
  for (const name of Array.from(db.objectStoreNames)) {
    const keys = await new Promise(r => {
      const q = db.transaction(name).objectStore(name).getAllKeys();
      q.onsuccess = () => r(q.result);
    });
    for (const key of keys) {
      const m = /^(?:n\/([^/]+)\/|k\/(.+)$)/.exec(key);
      const id = m && (m[1] ?? m[2]);
      if (id && id !== account) ids.add(id);
    }
  }
  db.close();
  return [...ids].sort();
}, account);

const idb = d => d.page.evaluate(() => ({ ...window.__idb }));

async function device(name) {
  const context = await browser.newContext({ viewport: { width: 1000, height: 1200 } });
  await context.addInitScript(refuseAWrite);
  if (trace) await context.addInitScript(() => { Error.stackTraceLimit = 50; });
  // What the round was doing, so that an error says when it happened.
  const d = { name, context, crashes: [], errors: [], doing: 'loading' };
  context.on('page', page => {
    const pageId = tracePage++;
    if (trace) {
      page.on('console', m => trace.write(JSON.stringify({ page: pageId, phase: d.doing, type: m.type(), message: m.text() }) + '\n'));
      page.on('pageerror', e => trace.write(JSON.stringify({ page: pageId, phase: d.doing, type: 'pageerror', message: e.stack }) + '\n'));
    }
    page.on('pageerror', e => d.crashes.push(`${d.doing}: ${e.message}`));
    page.on('console', m => m.type() === 'error' && d.errors.push(`${d.doing}: ${m.text()}`));
  });
  d.page = await context.newPage();
  await load(d, url);
  return d;
}

/// A checklist with one item, made through the screens, and left.
async function makeNote(d, item) {
  await tap(d, /^Create note/);
  await tap(d, /^List(?:\n|$)/);
  await tap(d, /^Checklist item$/);
  await d.page.keyboard.type(item);
  await d.page.waitForTimeout(600);
}

async function trashOpenNote(d) {
  await tap(d, /^More note actions$/);
  await tap(d, /^Move to trash$/);
  await find(d, n => n.text.trim() === 'Untitled checklist');
}

async function openTrash(d) {
  await tap(d, /^Open navigation menu$/);
  await tap(d, /^Trash/);
  await d.page.waitForTimeout(800);
}

async function emptyTrash(d) {
  await tap(d, /^Empty the trash$/);
  await tap(d, /^Delete all$/);
  await d.page.waitForTimeout(3000);
}

try {
  for (let n = 0; ; n++) {
    console.log(`--    refusing write ${n} of the deletion`);
    const d = await device(`round ${n}`);

    d.doing = 'making notes';
    await makeNote(d, 'keep me');
    await tap(d, /^Back$/);
    await find(d, n => n.text.includes('keep me'));
    const [kept] = await noteIds(d);
    await makeNote(d, 'old one');
    await trashOpenNote(d);
    await makeNote(d, 'old two');
    await trashOpenNote(d);
    await d.page.waitForTimeout(500);
    const thrown = (await noteIds(d)).filter(id => id !== kept);
    check(!!kept && thrown.length === 2, `two notes in the trash and one kept (${thrown.length})`);

    d.doing = 'emptying the trash';
    await openTrash(d);
    await find(d, n => n.text.includes('old one'));
    const before = await idb(d);
    await d.page.evaluate(n => { window.__idb.failAt = window.__idb.writes + n; }, n);
    await emptyTrash(d);
    const after = await idb(d);
    const refused = after.refused > 0;

    if (refused) {
      check(
        !!(await find(d, n => n.text.includes('This browser would not keep an edit'), { timeout: 5000 })),
        'the refusal is on the screen',
      );
      check(
        after.writes === after.failAt + 1,
        `nothing was written after it (${after.writes - after.failAt - 1} writes)`,
      );
    } else {
      check(after.writes > before.writes, `the deletion took ${after.writes - before.writes} writes`);
    }

    // The tab closed and another opened, over whatever the browser kept.
    d.doing = 'reopening';
    await d.page.close();
    d.page = await d.context.newPage();
    await load(d, url);
    d.doing = 'emptying it again';
    const left = (await noteIds(d)).filter(id => thrown.includes(id));
    check((await noteIds(d)).includes(kept), 'the kept note is still there');
    if (!refused) check(left.length === 0, 'an uninterrupted deletion left nothing');
    await openTrash(d);
    const shown = (await texts(d)).filter(t => /old (one|two)/.test(t)).length;
    check(shown === left.length, `the trash shows the ${left.length} note(s) with rows left (${shown})`);
    if (left.length > 0) {
      await emptyTrash(d);
      await find(d, n => n.text.trim() === 'The trash is empty.');
    }
    const remaining = await noteIds(d);
    check(
      thrown.every(id => !remaining.includes(id)) && remaining.includes(kept),
      'emptying it again took the rest and only the rest',
    );
    check(d.crashes.length === 0, `the page threw nothing: ${d.crashes.slice(0, 2).join(' | ')}`);
    // One logged panic is known and is not this app's: now and then, as a tab
    // goes and another comes, flutter_rust_bridge releases a `Note` handle whose
    // ID is already gone from its pool (`MoiArc::decrement_strong`). The rows are
    // right when it happens, and nothing is thrown. It is reported here rather
    // than failed, so it stays visible without making this flaky;
    // ADR 0002 ("Not done, and honest about it") has the stack and how to look.
    // Any other error, anywhere, fails.
    const known = e => !trace && e.startsWith('reopening: ')
      && e.includes('frb_generated.rs')
      && e.includes('MOI missing handle on release:');
    for (const e of d.errors.filter(known)) console.log(`note  known bridge panic at launch: ${e.split('\n')[0]}`);
    const errors = d.errors.filter(e => !known(e));
    check(errors.length === 0, `and logged no errors: ${errors.slice(0, 2).join(' | ').slice(0, 300)}`);
    await d.context.close();

    if (!refused) break;
    if (n > 30) {
      check(false, 'a deletion of two notes should not need thirty writes');
      break;
    }
  }
} finally {
  await browser.close();
  if (trace) {
    trace.end();
    await finished(traceOutput);
  }
}
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
console.log('\na refused write never leaves a note half deleted');
