/// Google Keep Takeout import: what a text note, a checklist and the records
/// that are left out turn into before anything reaches the core.
library;

import 'dart:convert';

import 'package:easynotes/core/keep_import.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test(
    'a text note keeps its title, body, labels, colour and archive flag',
    () {
      final result = parseKeepExport({
        'Shopping.json': jsonEncode({
          'title': 'Shopping',
          'textContent': 'Milk\neggs',
          'labels': [
            {'name': 'Home'},
            {'name': 'Errands'},
          ],
          'color': 'DEFAULT',
          'isArchived': true,
          'createdTimestampUsec': 1600000000000000,
          'userEditedTimestampUsec': 1600000001000000,
        }),
      });

      expect(result.skipped, isEmpty);
      final note = result.notes.single;
      expect(note.title, 'Shopping');
      expect(note.body, 'Milk\neggs');
      expect(note.kind, NoteKind.text);
      expect(note.items, isEmpty);
      expect(note.labels, ['Home', 'Errands']);
      expect(note.colour, isNull);
      expect(note.archived, isTrue);
      expect(
        note.created,
        DateTime.fromMicrosecondsSinceEpoch(1600000000000000, isUtc: true),
      );
      expect(
        note.edited,
        DateTime.fromMicrosecondsSinceEpoch(1600000001000000, isUtc: true),
      );
    },
  );

  test('a checklist keeps its rows and which are ticked', () {
    final result = parseKeepExport({
      'Groceries.json': jsonEncode({
        'title': 'Groceries',
        'listContent': [
          {'text': 'Milk', 'isChecked': true},
          {'text': 'Eggs', 'isChecked': false},
        ],
        'color': 'GRAY',
      }),
    });

    expect(result.skipped, isEmpty);
    final note = result.notes.single;
    expect(note.kind, NoteKind.checklist);
    expect(note.items.map((item) => item.text), ['Milk', 'Eggs']);
    expect(note.items.map((item) => item.checked), [true, false]);
    expect(note.colour, NoteColour.grey);
  });

  test('a trashed note is skipped with a reason', () {
    final result = parseKeepExport({
      'Old.json': jsonEncode({
        'title': 'Old',
        'textContent': 'gone',
        'isTrashed': true,
      }),
    });

    expect(result.notes, isEmpty);
    expect(result.skipped.single.file, 'Old.json');
    expect(result.skipped.single.reason, 'Trashed in Google Keep.');
  });

  test('a record with only attachments is skipped as unsupported', () {
    final result = parseKeepExport({
      'Drawing.json': jsonEncode({
        'attachments': [
          {'filePath': 'drawing.png'},
        ],
      }),
    });

    expect(result.notes, isEmpty);
    expect(
      result.skipped.single.reason,
      'Nothing to import: no title, text or checklist items.',
    );
  });

  test('unreadable records are reported, not thrown', () {
    final result = parseKeepExport({
      'Broken.json': '{"title": "Broken",',
      'Wrong.json': jsonEncode({'title': 'Wrong', 'listContent': 'not a list'}),
    });

    expect(result.notes, isEmpty);
    expect(result.skipped.map((skipped) => skipped.reason), [
      'Not valid JSON.',
      'Malformed Google Keep note.',
    ]);
  });
}
