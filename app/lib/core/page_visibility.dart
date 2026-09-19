/// When the page is going away, for a store that has queued writes.
///
/// The one thing in the app that knows about the browser's page lifecycle, kept
/// apart from `indexed_db.dart` because it is not about a database: it is about
/// the tab. A device with a disk has no equivalent and needs none — its store
/// has already written before the edit returned.
library;

import 'dart:js_interop';

import 'package:web/web.dart' as web;

/// Call [onHidden] whenever this page stops being visible, and return the
/// function that stops listening.
///
/// `visibilitychange` and not `beforeunload`: a tab is very often closed, or
/// swapped away from on a phone, without `beforeunload` ever firing, and
/// `visibilitychange` is the last event a browser reliably delivers. It fires on
/// every switch to another tab as well, which costs nothing — the queue is
/// usually empty and emptying an empty queue is a comparison.
void Function() whenPageHidden(void Function() onHidden) {
  final listener = ((web.Event _) {
    if (web.document.visibilityState == 'hidden') onHidden();
  }).toJS;
  web.document.addEventListener('visibilitychange', listener);
  return () => web.document.removeEventListener('visibilitychange', listener);
}
