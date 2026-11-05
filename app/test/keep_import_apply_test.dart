/// Google Keep Takeout import: what a parsed export turns into in the library,
/// and that one refused note does not take the rest with it.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/keep_import.dart';
import 'package:easynotes/core/keep_import_apply.dart';
import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test(
    'a text note and a checklist are made as the export describes',
    () async {
      final library = FakeNoteLibrary();
      addTearDown(library.dispose);

      final result = await applyKeepImport(
        library,
        const KeepImport(
          notes: [
            KeepNote(
              title: 'Shopping',
              body: 'Milk\neggs',
              kind: NoteKind.text,
              labels: ['Home', 'Errands'],
              colour: NoteColour.teal,
              archived: true,
            ),
            KeepNote(
              title: 'Groceries',
              body: '',
              kind: NoteKind.checklist,
              items: [
                KeepItem(text: 'Bread', checked: true),
                KeepItem(text: 'Jam', checked: false),
              ],
            ),
          ],
          skipped: [],
        ),
      );

      expect(result.imported, 2);
      expect(result.skipped, isEmpty);

      final text = library.notes.firstWhere((note) => note.title == 'Shopping');
      expect(text.kind, NoteKind.text);
      expect(text.colour, NoteColour.teal);
      expect(text.archived, isTrue);
      expect(text.tags.map((tag) => tag.name), ['Errands', 'Home']);
      expect(library.core(text.id).current.body, 'Milk\neggs');

      final list = library.notes.firstWhere(
        (note) => note.title == 'Groceries',
      );
      expect(list.kind, NoteKind.checklist);
      expect(library.core(list.id).current.items.map((item) => item.text), [
        'Bread',
        'Jam',
      ]);
      expect(library.core(list.id).current.items.map((item) => item.checked), [
        true,
        false,
      ]);

      // Every core this import opened was let go of again.
      expect(library.core(text.id).disposals, 1);
      expect(library.core(list.id).disposals, 1);
    },
  );

  test('a failed note is reported and the next one still goes in', () async {
    final library = _FailingLibrary();
    addTearDown(library.dispose);

    final result = await applyKeepImport(
      library,
      const KeepImport(
        notes: [
          KeepNote(title: 'First', body: 'lost', kind: NoteKind.text),
          KeepNote(title: 'Second', body: 'kept', kind: NoteKind.text),
        ],
        skipped: [KeepSkipped(file: 'Broken.json', reason: 'Not valid JSON.')],
      ),
    );

    expect(result.imported, 1);
    expect(result.skipped.map((skipped) => skipped.file), [
      'Broken.json',
      'First',
    ]);
    expect(result.skipped.last.reason, contains('no room'));

    final kept = library.notes.where((note) => !note.trashed).single;
    expect(kept.title, 'Second');
    expect(library.core(kept.id).current.body, 'kept');
  });
}

/// Refuses to open its first note, as a core that cannot take one would.
class _FailingLibrary extends FakeNoteLibrary {
  var _fail = true;

  @override
  Future<NoteCore> openNote(String noteId) async {
    if (_fail) {
      _fail = false;
      throw StateError('no room');
    }
    return super.openNote(noteId);
  }
}
