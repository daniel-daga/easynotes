/// The address bar, for the one thing the app changes in it.
///
/// A Web Share Target arrives as the app's own URL with the shared text in its
/// query (`shared_text.dart`). Once that has become a note the query has to go,
/// or a reload — or reopening the installed app from its history — would make
/// the same note again.
library;

import 'package:web/web.dart' as web;

/// Replace the current history entry with the same address minus its query,
/// keeping the fragment an invite link lives in.
void dropQuery() {
  final here = Uri.parse(web.window.location.href);
  if (here.query.isEmpty) return;
  final fragment = here.hasFragment ? '#${here.fragment}' : '';
  web.window.history.replaceState(
    null,
    '',
    '${here.origin}${here.path}$fragment',
  );
}
