// Compile with dart compile js; driven by tools/web-smoke/store-ownership.mjs.
import 'dart:js_interop';
import 'dart:typed_data';

import 'package:easynotes/core/browser_store.dart';
import 'package:easynotes/core/indexed_db.dart';
import 'package:easynotes/src/rust/api/notes.dart';

@JS('storeOpen')
external set storeOpen(JSFunction value);
@JS('storeWrite')
external set storeWrite(JSFunction value);
@JS('storeRead')
external set storeRead(JSFunction value);
@JS('storeClose')
external set storeClose(JSFunction value);

BrowserStore? store;
void main() {
  storeOpen = ((JSString name) => open(name.toDart).toJS).toJS;
  storeWrite = ((JSNumber value) => write(value.toDartInt).toJS).toJS;
  storeRead = (() => read().toJS).toJS;
  storeClose = (() => close().toJS).toJS;
}

Future<JSAny?> open(String name) async {
  store = await openIndexedDb(name);
  return null;
}

Future<JSAny?> write(int value) async {
  await store!.apply(
    StorageBatch(
      area: StorageArea.notes,
      writes: [
        StorageWrite(
          kind: StorageWriteKind.put,
          key: 'edit-$value',
          value: Uint8List.fromList([value]),
        ),
      ],
    ),
  );
  return null;
}

Future<JSAny?> read() async => (await store!.rows(
  StorageArea.notes,
)).map((row) => row.value.single.toJS).toList().toJS;

Future<JSAny?> close() async {
  await store!.close();
  return null;
}
