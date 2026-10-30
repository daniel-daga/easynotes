import 'note_view.dart';

/// Text-only interchange for a note. Titles and personal note metadata are
/// deliberately excluded.
///
/// A checklist comes out as Markdown task lines, and a section heading as a
/// `# Line` — the same two forms `parse_list_line` reads on the way back in and
/// `looksLikeTaskList` recognises, so a list shared out and back keeps its
/// sections ([ADR 0037](../../docs/decisions.md#adr-0037)).
String contentForSharing(NoteView note) {
  if (note.kind == NoteKind.text) return note.body;
  return note.items
      .map(
        (item) => item.heading
            ? '# ${item.text}'
            : '[${item.checked ? 'x' : ' '}] ${item.text}',
      )
      .join('\n');
}
