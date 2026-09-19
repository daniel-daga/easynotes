/// Where a browser keeps this origin's notes.
///
/// The Rust core does not reach IndexedDB itself, and the reason is the same one
/// that puts the master key in [MasterKeyStore] rather than in the core: the
/// platform is Dart's. There is a sharper reason here too. On the web,
/// flutter_rust_bridge runs a synchronous bridge method on the page's own thread
/// and every other one on a Web Worker, while an `IDBDatabase` belongs to the
/// thread that opened it — so a handle held in Rust would be reachable from some
/// of the methods that write through it and not from the others. What crosses
/// the bridge instead is bytes: rows in at launch, batches of writes out.
///
/// The core's half of this is `easynotes_core::store::web`, and that is where
/// everything with a rule in it lives — the key layout, the sealing, which
/// writes have to land together. What is left down here is applying a batch and
/// reading every row back, which is why this file has no decisions in it and no
/// knowledge of what a key means.
library;

import 'dart:async';
import 'dart:typed_data';

import '../src/rust/api/notes.dart' as rust;
import 'indexed_db_stub.dart'
    if (dart.library.js_interop) 'indexed_db.dart'
    as platform;

/// A key-value store with two areas, which is all IndexedDB is from here.
abstract interface class BrowserStore {
  /// Every row of one area. Read once, at launch.
  Future<List<rust.StorageRow>> rows(rust.StorageArea area);

  /// Apply one batch, **in one transaction**.
  ///
  /// A batch is what the core says must not be applied by halves — the two
  /// steps of a compaction, or everything a forgotten note had — so splitting
  /// one across two transactions would reintroduce the crash windows the store
  /// is written to avoid. It completes when the transaction does, not when the
  /// writes are issued, because the caller uses that to know the edit is kept.
  Future<void> apply(rust.StorageBatch batch);

  Future<void> close();
}

/// The database this origin's notes are in, opened or created.
///
/// Throws outside a browser, where there is no such thing — the native build
/// opens a directory instead and never calls this.
Future<BrowserStore> openBrowserStore(String name) =>
    platform.openIndexedDb(name);

/// A [BrowserStore] that is a map, for tests and for a tab whose browser will
/// not give us a database.
///
/// The second of those is not a hypothetical: IndexedDB is unavailable in some
/// private-browsing modes and can be switched off outright, and a tab that
/// cannot keep notes should still be able to take them. It behaves exactly like
/// the real one and forgets everything when the tab closes, which is what the
/// web build did before any of this.
class MemoryBrowserStore implements BrowserStore {
  final _areas = <rust.StorageArea, Map<String, Uint8List>>{};

  Map<String, Uint8List> _area(rust.StorageArea area) =>
      _areas.putIfAbsent(area, () => {});

  @override
  Future<List<rust.StorageRow>> rows(rust.StorageArea area) async => [
    for (final entry in _area(area).entries)
      rust.StorageRow(key: entry.key, value: entry.value),
  ];

  @override
  Future<void> apply(rust.StorageBatch batch) async {
    final rows = _area(batch.area);
    for (final write in batch.writes) {
      switch (write.kind) {
        case rust.StorageWriteKind.put:
          rows[write.key] = write.value;
        case rust.StorageWriteKind.delete:
          rows.remove(write.key);
        case rust.StorageWriteKind.deletePrefix:
          rows.removeWhere((key, _) => key.startsWith(write.key));
      }
    }
  }

  @override
  Future<void> close() async {}
}
