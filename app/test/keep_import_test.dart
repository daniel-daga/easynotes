/// Google Keep Takeout import: what a text note, a checklist and the records
/// that are left out turn into before anything reaches the core.
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:archive/archive.dart';
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

  test('a Takeout zip is read without extracting it', () {
    final archive = Archive()
      ..addFile(
        ArchiveFile.string(
          'Takeout/Keep/Text.json',
          jsonEncode({'title': 'Text', 'textContent': 'hello'}),
        ),
      )
      ..addFile(
        ArchiveFile.string(
          'Keep/List.json',
          jsonEncode({
            'title': 'List',
            'listContent': [
              {'text': 'milk', 'isChecked': true},
            ],
          }),
        ),
      )
      ..addFile(
        ArchiveFile.string('Takeout/Keep/Text.html', '<html>not a note</html>'),
      );

    final result = parseKeepFile(
      'takeout.zip',
      Uint8List.fromList(ZipEncoder().encode(archive)),
    );

    expect(result.notes.map((note) => note.title), ['Text', 'List']);
    expect(result.notes.last.items.single.checked, isTrue);
    expect(result.skipped, isEmpty);
  });

  test('a zip with no Keep JSON records says what is missing', () {
    final archive = Archive()
      ..addFile(ArchiveFile.string('Takeout/Keep/Text.html', '<html></html>'));

    final result = parseKeepFile(
      'takeout.zip',
      Uint8List.fromList(ZipEncoder().encode(archive)),
    );

    expect(result.notes, isEmpty);
    expect(
      result.skipped.single.reason,
      'No Google Keep JSON records in the archive.',
    );
  });

  test('a picked file over the limit is refused before it is parsed', () {
    final result = parseKeepFile(
      'huge.json',
      Uint8List.fromList(utf8.encode('{"title":"Text"}')),
      maxFileBytes: 4,
    );
    expect(result.notes, isEmpty);
    expect(result.skipped.single.reason, contains('import limit'));
  });

  test('one oversized JSON entry does not hide the rest of the archive', () {
    final archive = Archive()
      ..addFile(
        ArchiveFile.string(
          'Takeout/Keep/Text.json',
          jsonEncode({'title': 'Text', 'textContent': 'hello'}),
        ),
      )
      ..addFile(
        ArchiveFile.string(
          'Takeout/Keep/Large.json',
          jsonEncode({'title': 'Large', 'textContent': 'x' * 40}),
        ),
      );
    final result = parseKeepFile(
      'takeout.zip',
      Uint8List.fromList(ZipEncoder().encode(archive)),
      maxEntryBytes: 60,
    );
    expect(result.notes.single.title, 'Text');
    expect(result.skipped.single.file, contains('Large.json'));
  });

  test('a broken zip is reported, not thrown', () {
    final result = parseKeepFile(
      'takeout.zip',
      Uint8List.fromList([0x50, 0x4b, 0x03, 0x04, 0x00]),
    );

    expect(result.notes, isEmpty);
    expect(result.skipped.single.reason, 'Not a readable zip archive.');
  });

  test('a single extracted JSON file is read too', () {
    final result = parseKeepFile(
      'Text.json',
      Uint8List.fromList(
        utf8.encode(jsonEncode({'title': 'Text', 'textContent': 'hello'})),
      ),
    );

    expect(result.notes.single.title, 'Text');
    expect(result.skipped, isEmpty);
  });
}
