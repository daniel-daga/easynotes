// What opening a device costs in a browser, at three sizes.
//
// The gap ADR 0011 left and slice 2's items 4 and 9 both name: `just
// bench-library` measures SQLite on a disk, and a tab reads IndexedDB and
// unseals in WebAssembly. Neither half of that is the same work, so the native
// table cannot be read across — and the number matters, because reading the
// whole database at open is the bet ADR 0011 makes.
//
// A launch is three things and this times them apart:
//
//   read    every row out of IndexedDB, the way `indexed_db.dart` does it
//   open    `WebStore::from_rows` and `NoteCore::open`, which replays the
//           account document before a note is read
//   list    `NoteCore::notes`, which unseals one index entry per note
//
// The rows are seeded by the same WebAssembly module, from the same filling
// `core/examples/bench_library.rs` uses, and then written into a real IndexedDB
// — so what `read` measures is a browser reading a database it is actually
// holding, not a fixture handed to it.
//
//     node tools/web-bench/bench.mjs <url>
import { chromium } from 'playwright-core';

const url = process.argv[2] ?? 'http://127.0.0.1:8100/';
// As in `web-smoke`: no browser of its own, because a CI runner or a
// developer's machine already has one.
const executablePath = process.env.CHROME_EXECUTABLE;

/// How many notes a device holds. The same three the native table has, so the
/// two can be put side by side.
const COUNTS = [10, 50, 200];

/// Each size is measured this many times and the fastest is kept. A benchmark
/// in a browser on a shared machine has a long tail — a GC, another tab, the
/// runner's neighbour — and the fastest run is the one least contaminated by
/// something that is not this code.
const RUNS = 3;

const browser = await chromium.launch({
  ...(executablePath ? { executablePath } : { channel: 'chrome' }),
  args: ['--no-sandbox'],
});
const page = await (await browser.newContext()).newPage();
const crashes = [];
page.on('pageerror', e => crashes.push(e.message));

await page.goto(url, { waitUntil: 'load' });
await page.waitForFunction(() => window.bench !== undefined, { timeout: 120_000 });

/// One launch, measured. Everything below happens inside the page: the clock is
/// `performance.now()` there rather than a round trip from here, because a
/// round trip is milliseconds and so is the thing being timed.
const measure = notes => page.evaluate(async count => {
  const database = 'easynotes-bench';
  const areas = ['notes', 'keys'];

  await new Promise((resolve, reject) => {
    const request = indexedDB.deleteDatabase(database);
    request.onsuccess = resolve;
    request.onerror = () => reject(request.error);
    request.onblocked = resolve;
  });
  const db = await new Promise((resolve, reject) => {
    const request = indexedDB.open(database, 1);
    request.onupgradeneeded = () => {
      for (const area of areas) request.result.createObjectStore(area);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });

  // Seed a device and put its rows where a second launch would find them.
  const seeded = window.bench.seed(count);
  for (const area of areas) {
    const transaction = db.transaction(area, 'readwrite');
    const store = transaction.objectStore(area);
    for (const [key, value] of seeded.get(area)) store.put(value, key);
    await new Promise((resolve, reject) => {
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
    });
  }

  // 1. Reading the database. `getAllKeys` and `getAll` per object store, which
  //    is what `IndexedDbStore.rows` does and for the reason it says: both are
  //    ordered by key, so they line up.
  const startRead = performance.now();
  const read = {};
  for (const area of areas) {
    const store = db.transaction(area, 'readonly').objectStore(area);
    const keys = await new Promise(r => {
      const q = store.getAllKeys();
      q.onsuccess = () => r(q.result);
    });
    const values = await new Promise(r => {
      const q = store.getAll();
      q.onsuccess = () => r(q.result);
    });
    read[area] = new Map(keys.map((k, i) => [k, values[i]]));
  }
  const readMs = performance.now() - startRead;

  // 2. Opening the device.
  const startOpen = performance.now();
  window.bench.open(seeded.get('master'), read.notes, read.keys);
  const openMs = performance.now() - startOpen;

  // 3. Listing what it holds.
  const startList = performance.now();
  const listed = window.bench.list();
  const listMs = performance.now() - startList;

  window.bench.close();
  db.close();

  const bytes = areas.reduce(
    (total, area) => total + [...read[area].values()].reduce((n, v) => n + v.byteLength, 0),
    0,
  );
  const rows = areas.reduce((total, area) => total + read[area].size, 0);
  return { readMs, openMs, listMs, listed, rows, bytes };
}, notes);

const table = [];
for (const notes of COUNTS) {
  let best = null;
  for (let run = 0; run < RUNS; run++) {
    const result = await measure(notes);
    if (result.listed !== notes) {
      console.error(`FAIL  ${notes} notes seeded, ${result.listed} listed`);
      process.exit(1);
    }
    const total = result.readMs + result.openMs + result.listMs;
    if (best === null || total < best.readMs + best.openMs + best.listMs) best = result;
  }
  table.push({ notes, ...best });
}

const ms = n => `${n.toFixed(1)} ms`;
console.log();
console.log('| Notes | Rows | Bytes read | Reading IndexedDB | Opening the device | Listing the notes | Total |');
console.log('| --- | --- | --- | --- | --- | --- | --- |');
for (const row of table) {
  console.log(
    `| ${row.notes} | ${row.rows} | ${(row.bytes / 1024).toFixed(0)} KiB | ${ms(row.readMs)} | ` +
      `${ms(row.openMs)} | ${ms(row.listMs)} | **${ms(row.readMs + row.openMs + row.listMs)}** |`,
  );
}
console.log();
for (const row of table) {
  console.log(
    `${String(row.notes).padStart(4)} notes: ` +
      `${((row.readMs + row.openMs + row.listMs) / row.notes).toFixed(2)} ms a note`,
  );
}

await browser.close();
if (crashes.length) {
  console.error(`\nthe page threw: ${crashes.slice(0, 3).join(' | ')}`);
  process.exit(1);
}
