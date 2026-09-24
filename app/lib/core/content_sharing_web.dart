import 'dart:js_interop';

import 'package:web/web.dart' as web;

Future<bool> shareContent(String text) async {
  final navigator = web.window.navigator;
  final data = web.ShareData(text: text);
  try {
    if (!navigator.canShare(data)) return false;
    await navigator.share(data).toDart;
    return true;
  } on web.DOMException catch (error) {
    if (error.name == 'AbortError') return true;
    return false;
  } catch (_) {
    return false;
  }
}
