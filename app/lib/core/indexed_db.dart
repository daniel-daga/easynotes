/// [BrowserStore] over the browser's own IndexedDB.
///
/// The only file in the app that knows what IndexedDB is. It has no rules in it:
/// the key layout, the sealing and which writes must land together are all in
/// `easynotes_core::store::web`, where `cargo test` can reach them without a
/// browser. What is here is three operations — open, read everything, apply a
/// batch — and the ceremony a request-and-event API needs to look like a
/// `Future`.
library;

import 'dart:async';
import 'dart:js_interop';

import 'package:web/web.dart' as web;

import '../src/rust/api/notes.dart' as rust;
import 'browser_store.dart';

/// Bumped when the *object stores* change, which is a different question from
/// the row layout inside them — that one is the core's `SCHEMA_VERSION`, kept in
/// a row of its own, and the core refuses a database a newer build wrote.
const _version = 1;

const _notes = 'notes';
const _keys = 'keys';

String _area(rust.StorageArea area) => switch (area) {
  rust.StorageArea.notes => _notes,
  rust.StorageArea.keys => _keys,
};

Future<BrowserStore> openIndexedDb(String name) async {
  // The core caches both rows and device counters. Transaction-level locking
  // cannot protect two copies of those caches; ownership lasts until close.
  final acquired = Completer<void>();
  final release = Completer<JSAny?>();
  final held = web.window.navigator.locks
      .request(
        'easynotes-database:$name',
        ((web.Lock lock) {
          acquired.complete();
          return release.future.toJS;
        }).toJS,
      )
      .toDart;
  unawaited(
    held.then<void>(
      (_) {},
      onError: (Object error, StackTrace stack) {
        if (!acquired.isCompleted) acquired.completeError(error, stack);
      },
    ),
  );
  await acquired.future;
  Future<void> unlock() async {
    if (!release.isCompleted) release.complete(null);
    await held;
  }

  try {
    return IndexedDbStore._(await _open(name), unlock);
  } on Object {
    await unlock();
    rethrow;
  }
}

class IndexedDbStore implements BrowserStore {
  IndexedDbStore._(this._db, this._unlock);

  final web.IDBDatabase _db;
  final Future<void> Function() _unlock;

  @override
  Future<List<rust.StorageRow>> rows(rust.StorageArea area) async {
    final store = _db
        .transaction(_area(area).toJS, 'readonly')
        .objectStore(_area(area));
    // Both are ordered by key and cover the same rows, so they line up. A
    // cursor would say the same thing one round trip at a time.
    final keys = await _request(store.getAllKeys());
    final values = await _request(store.getAll());
    final keyList = (keys as JSArray).toDart;
    final valueList = (values as JSArray).toDart;
    return [
      for (var i = 0; i < keyList.length; i++)
        rust.StorageRow(
          key: (keyList[i] as JSString).toDart,
          value: (valueList[i] as JSUint8Array).toDart,
        ),
    ];
  }

  @override
  Future<void> apply(rust.StorageBatch batch) async {
    final name = _area(batch.area);
    // One transaction for the batch, which is the whole of what the core asks
    // of this method.
    final transaction = _db.transaction(name.toJS, 'readwrite');
    final store = transaction.objectStore(name);
    for (final write in batch.writes) {
      switch (write.kind) {
        case rust.StorageWriteKind.put:
          store.put(write.value.toJS, write.key.toJS);
        case rust.StorageWriteKind.delete:
          store.delete(write.key.toJS);
        case rust.StorageWriteKind.deletePrefix:
          // `￿` is above every character a key of ours can hold — they are
          // ASCII — so the range is exactly the keys beginning with the prefix.
          store.delete(
            web.IDBKeyRange.bound(write.key.toJS, '${write.key}￿'.toJS),
          );
      }
    }
    await _done(transaction);
  }

  @override
  Future<void> close() async {
    _db.close();
    await _unlock();
  }
}

Future<web.IDBDatabase> _open(String name) {
  final opened = Completer<web.IDBDatabase>();
  final request = web.window.indexedDB.open(name, _version);
  request.onupgradeneeded = ((web.IDBVersionChangeEvent _) {
    final db = request.result as web.IDBDatabase;
    for (final store in [_notes, _keys]) {
      if (!db.objectStoreNames.contains(store)) db.createObjectStore(store);
    }
  }).toJS;
  request.onsuccess = ((web.Event _) {
    opened.complete(request.result as web.IDBDatabase);
  }).toJS;
  request.onerror = ((web.Event _) {
    opened.completeError(
      StateError(
        'this browser would not open a database: ${request.error?.message}',
      ),
    );
  }).toJS;
  return opened.future;
}

Future<JSAny?> _request(web.IDBRequest request) {
  final done = Completer<JSAny?>();
  request.onsuccess = ((web.Event _) => done.complete(request.result)).toJS;
  request.onerror = ((web.Event _) => done.completeError(
    StateError('${request.error?.message}'),
  )).toJS;
  return done.future;
}

Future<void> _done(web.IDBTransaction transaction) {
  final done = Completer<void>();
  transaction.oncomplete = ((web.Event _) => done.complete()).toJS;
  // An abort is a failed write and not a quiet one: the caller has already been
  // handed these writes and will not be handed them again, so it has to hear.
  //
  // A refused request fires both of these, `error` and then `abort`, and the
  // first one wins. Completing twice would throw inside the browser's event
  // handler, where nobody is listening.
  void failed(String message) {
    if (!done.isCompleted) done.completeError(StateError(message));
  }

  // The request that failed is what knows why; the transaction is only told
  // once it aborts, which is after this.
  transaction.onerror = ((web.Event event) => failed(
    '${(event.target as web.IDBRequest?)?.error?.message}',
  )).toJS;
  transaction.onabort = ((web.Event _) => failed(
    transaction.error?.message ?? 'the browser abandoned the write',
  )).toJS;
  return done.future;
}
