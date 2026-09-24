import 'note_view.dart';

/// Text-only interchange for a note. Titles and personal note metadata are
/// deliberately excluded.
String contentForSharing(NoteView note) {
  if (note.kind == NoteKind.text) return note.body;
  return note.items
      .map((item) => '[${item.checked ? 'x' : ' '}] ${item.text}')
      .join('\n');
}
