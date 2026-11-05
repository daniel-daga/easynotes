/// The half of a Google Keep import that creates notes.
///
/// [parseKeepExport] reads the export and creates nothing; this talks to the
/// library one note at a time, so a record the core refuses is reported beside
/// the ones that worked instead of losing the rest.
library;

import 'keep_import.dart';
import 'note_library.dart';

/// How an import into a library went.
class KeepImportResult {
  const KeepImportResult({required this.imported, required this.skipped});

  final int imported;

  /// Everything left out: what the parser skipped, and any note that failed
  /// while it was being made.
  final List<KeepSkipped> skipped;
}

/// Create every note in [import] on this device.
Future<KeepImportResult> applyKeepImport(
  NoteLibrary library,
  KeepImport import,
) async {
  final skipped = [...import.skipped];
  var imported = 0;
  for (final note in import.notes) {
    String? id;
    try {
      id = await library.createNote(kind: note.kind);
      await _fill(library, id, note);
      if (note.colour case final colour?) await library.setColour(id, colour);
      for (final label in note.labels) {
        await library.addTag(id, label);
      }
      if (note.archived) await library.setArchived(id, true);
      imported++;
    } on Object catch (error) {
      // A note left at the first few edits reads as a real note, so it goes to
      // the trash where it can be recovered rather than being deleted.
      if (id != null) {
        try {
          await library.setTrashed(id, true);
        } on Object {
          // The failure worth reporting is the one that stopped the note.
        }
      }
      skipped.add(
        KeepSkipped(
          file: note.title.isEmpty ? 'Untitled note' : note.title,
          reason: 'Could not import: $error',
        ),
      );
    }
  }
  return KeepImportResult(imported: imported, skipped: skipped);
}

Future<void> _fill(NoteLibrary library, String id, KeepNote note) async {
  final core = await library.openNote(id);
  try {
    await core.setTitle(note.title);
    if (note.kind == NoteKind.text) {
      await core.editBody('', note.body);
    } else {
      for (final item in note.items) {
        final itemId = await core.addItem(item.text);
        if (item.checked) await core.setChecked(itemId, true);
      }
    }
  } finally {
    await core.dispose();
  }
}
