/// What [openIndexedDb] is outside a browser: nothing.
///
/// The native build opens a directory and never calls it, so this exists to
/// keep `dart:js_interop` out of a build that has no JavaScript in it — the
/// conditional import in `browser_store.dart` picks between the two.
library;

import 'browser_store.dart';

Future<BrowserStore> openIndexedDb(String name) => throw UnsupportedError(
  'there is no browser database outside a browser; this build opens a '
  'directory instead',
);
