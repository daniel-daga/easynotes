import 'dart:js_interop';

import 'package:web/web.dart' as web;

Future<bool> shareContent(String text) async {
  final navigator = web.window.navigator;
  final data = web.ShareData(text: text);
  try {
    if (!navigator.canShare(data)) return false;
    await navigator.share(data).toDart;
    return true;
  } catch (error) {
    // A person closing the share sheet is not a failure. Read by name rather
    // than by a type test, which cannot see a JS type the same way on every
    // compiler.
    return error.toString().contains('AbortError');
  }
}
