/// Text another app shared *into* EasyNotes, and the note it becomes.
///
/// The other half of `contentForSharing`: that one sends a note out as plain
/// text or Markdown task lines, and this takes the same format back in through
/// the platform's share sheet. See `docs/plan-text-interchange.md`.
///
/// Android delivers `ACTION_SEND` of `text/plain` to `MainActivity`, which
/// hands it over on the channels below — on a cold start through
/// [SharedTexts.initial], on a warm one through [SharedTexts.incoming]. An
/// installed web app receives it as a Web Share Target, which is a GET of the
/// app's own URL with the parts in the query ([sharedTextFromUri]). iOS needs a
/// share extension, which is its own target and waits for a Mac.
library;

import 'package:flutter/services.dart';

import 'note_library.dart';

/// What arrived: the text, and the subject if the sending app gave one — a
/// browser gives the page title, a mail app the subject line.
class SharedText {
  const SharedText({required this.text, this.subject});

  final String text;
  final String? subject;

  @override
  bool operator ==(Object other) =>
      other is SharedText && other.text == text && other.subject == subject;

  @override
  int get hashCode => Object.hash(text, subject);

  @override
  String toString() => 'SharedText($subject, $text)';
}

/// Put the parts a share can carry into one [SharedText], or `null` when there
/// is nothing in them. A URL shared on its own is text like any other; a URL
/// shared beside text that already contains it is not written twice.
SharedText? sharedTextFrom({String? subject, String? text, String? url}) {
  final body = [
    text?.trim() ?? '',
    if (url != null &&
        url.trim().isNotEmpty &&
        !(text?.contains(url.trim()) ?? false))
      url.trim(),
  ].where((part) => part.isNotEmpty).join('\n');
  if (body.isEmpty) return null;
  final title = subject?.trim();
  return SharedText(
    text: body,
    subject: title == null || title.isEmpty ? null : title,
  );
}

/// The query parameters `web/manifest.json`'s `share_target` names.
const shareTitleParam = 'share-title';
const shareTextParam = 'share-text';
const shareUrlParam = 'share-url';

/// A Web Share Target arrival, read off the URL the browser opened.
SharedText? sharedTextFromUri(Uri uri) {
  final query = uri.queryParameters;
  return sharedTextFrom(
    subject: query[shareTitleParam],
    text: query[shareTextParam],
    url: query[shareUrlParam],
  );
}

final _taskLine = RegExp(r'^\s*[-*+]\s+\[[ xX]\](\s|$)');

/// Whether every line with something on it is a Markdown task line — which is
/// what EasyNotes itself sends for a checklist, so a list shared out and back
/// in comes back a list. One plain line among them and it stays text: a
/// paragraph that happens to contain a checkbox is still a paragraph.
bool looksLikeTaskList(String text) {
  final lines = text.split('\n').where((line) => line.trim().isNotEmpty);
  return lines.isNotEmpty && lines.every(_taskLine.hasMatch);
}

/// Make the note [shared] describes, and return its ID.
///
/// Written as a text note and then switched, when it is a task list, rather
/// than parsed here: the switch is the core's, it already reads task markers
/// (`parse_task_line`), and one parser is the only way the two paths cannot
/// disagree about what a line means.
Future<String> createNoteFromSharedText(
  NoteLibrary library,
  SharedText shared,
) async {
  final id = await library.createNote(kind: NoteKind.text);
  final core = library.openNote(id);
  try {
    if (shared.subject != null) await core.setTitle(shared.subject!);
    await core.editBody('', shared.text);
    if (looksLikeTaskList(shared.text)) await core.setKind(NoteKind.checklist);
  } finally {
    await core.dispose();
  }
  return id;
}

/// Android's share-sheet arrivals. Nothing arrives anywhere else, and a
/// platform without the channel reads as "nothing was shared".
class SharedTexts {
  static const _methods = MethodChannel('eu.fraisl.easynotes/shared_text');
  static const _events = EventChannel('eu.fraisl.easynotes/shared_text/events');

  static SharedText? _decode(Object? value) {
    if (value is! Map) return null;
    return sharedTextFrom(
      subject: value['subject'] as String?,
      text: value['text'] as String?,
    );
  }

  /// What the app was launched with, if it was launched by a share.
  static Future<SharedText?> initial() async {
    try {
      return _decode(await _methods.invokeMethod<Object?>('getInitialText'));
    } on MissingPluginException {
      return null;
    }
  }

  /// Shares that arrive while the app is already running.
  static Stream<SharedText> get incoming => _events
      .receiveBroadcastStream()
      .map(_decode)
      .where((shared) => shared != null)
      .cast<SharedText>();
}
