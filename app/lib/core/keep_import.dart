/// Reading a Google Keep Takeout export.
///
/// This is the parsing half of an import: it turns the text of
/// `Takeout/Keep/*.json`, or a whole `.zip` of them, into plain values for the
/// UI and never creates a note.
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:archive/archive.dart';
import 'package:flutter/foundation.dart' show compute;

import 'note_view.dart';

/// One row of a Keep checklist.
class KeepItem {
  const KeepItem({required this.text, required this.checked});

  final String text;
  final bool checked;
}

/// A note read from the export, ready for the UI to show and then create.
class KeepNote {
  const KeepNote({
    required this.title,
    required this.body,
    required this.kind,
    this.items = const [],
    this.labels = const [],
    this.colour,
    this.archived = false,
    this.created,
    this.edited,
  });

  final String title;
  final String body;
  final NoteKind kind;
  final List<KeepItem> items;
  final List<String> labels;
  final NoteColour? colour;
  final bool archived;

  /// When Keep recorded the note, and when it last changed it. Null when the
  /// record does not carry the timestamp.
  final DateTime? created;
  final DateTime? edited;
}

/// A file that will not become a note, and why, in words a person can read.
class KeepSkipped {
  const KeepSkipped({required this.file, required this.reason});

  final String file;
  final String reason;
}

/// Everything one export gave: the notes to create, in file order, and what
/// was left out.
class KeepImport {
  const KeepImport({required this.notes, required this.skipped});

  final List<KeepNote> notes;
  final List<KeepSkipped> skipped;
}

/// Parse the extracted Keep records in [files], keyed by the file name to
/// report in [KeepImport.skipped].
///
/// A record that cannot be read is skipped with a reason rather than thrown:
/// one bad file must not hide the rest of someone's export.
KeepImport parseKeepExport(Map<String, String> files) {
  final notes = <KeepNote>[];
  final skipped = <KeepSkipped>[];
  for (final MapEntry(key: file, value: text) in files.entries) {
    final Object? json;
    try {
      json = jsonDecode(text);
    } on FormatException {
      skipped.add(KeepSkipped(file: file, reason: 'Not valid JSON.'));
      continue;
    }
    if (json is! Map<String, dynamic>) {
      skipped.add(KeepSkipped(file: file, reason: 'Not a Google Keep note.'));
      continue;
    }
    if (json['isTrashed'] == true) {
      skipped.add(KeepSkipped(file: file, reason: 'Trashed in Google Keep.'));
      continue;
    }
    try {
      final note = _note(json);
      if (note == null) {
        skipped.add(
          KeepSkipped(
            file: file,
            reason: 'Nothing to import: no title, text or checklist items.',
          ),
        );
      } else {
        notes.add(note);
      }
    } on TypeError {
      skipped.add(
        KeepSkipped(file: file, reason: 'Malformed Google Keep note.'),
      );
    }
  }
  return KeepImport(notes: notes, skipped: skipped);
}

/// The largest picked file this reads. Takeout keeps images beside the JSON;
/// an archive over this is extracted first.
const keepImportFileLimit = 200 << 20;

/// The largest one deflated JSON record this accepts.
const keepImportEntryLimit = 5 << 20;

/// Parse one file a person picked: a `.zip` straight from Takeout, or one of
/// the extracted `.json` records.
///
/// A zip is read in memory. That is the smallest thing that works for a
/// Takeout download; a multi-gigabyte archive should be extracted first.
KeepImport parseKeepFile(
  String name,
  Uint8List bytes, {
  int maxFileBytes = keepImportFileLimit,
  int maxEntryBytes = keepImportEntryLimit,
}) {
  if (bytes.lengthInBytes > maxFileBytes) {
    return KeepImport(
      notes: const [],
      skipped: [
        KeepSkipped(
          file: name,
          reason: 'Larger than the import limit; extract the JSON first.',
        ),
      ],
    );
  }
  if (!_isZip(name, bytes)) {
    return parseKeepExport({name: utf8.decode(bytes, allowMalformed: true)});
  }
  if (!_hasZipEnd(bytes)) {
    return KeepImport(
      notes: const [],
      skipped: [KeepSkipped(file: name, reason: 'Not a readable zip archive.')],
    );
  }

  final Archive archive;
  try {
    archive = ZipDecoder().decodeBytes(bytes);
  } on Object {
    return KeepImport(
      notes: const [],
      skipped: [KeepSkipped(file: name, reason: 'Not a readable zip archive.')],
    );
  }

  final files = <String, String>{};
  final skipped = <KeepSkipped>[];
  for (final file in archive.files) {
    if (!file.isFile || !_isKeepJson(file.name)) continue;
    if (file.size > maxEntryBytes) {
      skipped.add(
        KeepSkipped(file: file.name, reason: 'Larger than the import limit.'),
      );
      continue;
    }
    try {
      final content = file.content;
      if (content.lengthInBytes > maxEntryBytes) {
        skipped.add(
          KeepSkipped(file: file.name, reason: 'Larger than the import limit.'),
        );
        continue;
      }
      files[file.name] = utf8.decode(content, allowMalformed: true);
    } on Object {
      files[file.name] = '';
    }
  }
  if (files.isEmpty) {
    return KeepImport(
      notes: const [],
      skipped: skipped.isEmpty
          ? [
              KeepSkipped(
                file: name,
                reason: 'No Google Keep JSON records in the archive.',
              ),
            ]
          : skipped,
    );
  }
  final parsed = parseKeepExport(files);
  return KeepImport(
    notes: parsed.notes,
    skipped: [...skipped, ...parsed.skipped],
  );
}

/// [parseKeepFile] off the UI isolate on native. On the web `compute` runs on
/// the page's thread, but it is still one entry point for both.
Future<KeepImport> parseKeepFileInBackground(String name, Uint8List bytes) =>
    compute(_parsePickedKeepFile, (name, bytes));

KeepImport _parsePickedKeepFile((String, Uint8List) picked) =>
    parseKeepFile(picked.$1, picked.$2);

bool _isZip(String name, Uint8List bytes) =>
    name.toLowerCase().endsWith('.zip') ||
    (bytes.length >= 4 &&
        bytes[0] == 0x50 &&
        bytes[1] == 0x4b &&
        bytes[2] == 0x03 &&
        bytes[3] == 0x04);

/// A zip ends with an end-of-central-directory record. Its signature can sit up
/// to 64 KiB before the end when the archive has a comment.
bool _hasZipEnd(Uint8List bytes) {
  for (
    var i = bytes.length - 22;
    i >= 0 && i >= bytes.length - 0x10000 - 22;
    i--
  ) {
    if (bytes[i] == 0x50 &&
        bytes[i + 1] == 0x4b &&
        bytes[i + 2] == 0x05 &&
        bytes[i + 3] == 0x06) {
      return true;
    }
  }
  return false;
}

/// Takeout puts the records under `Takeout/Keep/`; a person who extracted and
/// re-zipped the folder has `Keep/` at the root. Both count.
bool _isKeepJson(String path) {
  final parts = path.toLowerCase().split('/');
  return path.toLowerCase().endsWith('.json') && parts.contains('keep');
}

KeepNote? _note(Map<String, dynamic> json) {
  final title = json['title'] as String? ?? '';
  final body = json['textContent'] as String? ?? '';
  final items = [
    for (final item
        in json['listContent'] as List<dynamic>? ?? const <dynamic>[])
      _item(item),
  ];
  if (title.isEmpty && body.isEmpty && items.isEmpty) return null;
  return KeepNote(
    title: title,
    body: body,
    kind: items.isEmpty ? NoteKind.text : NoteKind.checklist,
    items: items,
    labels: [
      for (final label in json['labels'] as List<dynamic>? ?? const <dynamic>[])
        if (label is Map<String, dynamic> && label['name'] is String)
          label['name'] as String,
    ],
    colour: _colour(json['color'] as String?),
    archived: json['isArchived'] == true,
    created: _timestamp(json['createdTimestampUsec']),
    edited: _timestamp(json['userEditedTimestampUsec']),
  );
}

KeepItem _item(Object? raw) {
  final item = raw as Map<String, dynamic>;
  return KeepItem(
    text: item['text'] as String? ?? '',
    checked: item['isChecked'] == true,
  );
}

NoteColour? _colour(String? keep) => switch (keep?.toUpperCase()) {
  'RED' => NoteColour.red,
  'ORANGE' => NoteColour.orange,
  'YELLOW' => NoteColour.yellow,
  'GREEN' => NoteColour.green,
  'TEAL' => NoteColour.teal,
  'BLUE' || 'DARK_BLUE' => NoteColour.blue,
  'PURPLE' => NoteColour.purple,
  'PINK' => NoteColour.pink,
  'BROWN' => NoteColour.brown,
  'GRAY' || 'GREY' => NoteColour.grey,
  // DEFAULT and WHITE are Keep's uncoloured card, and neither is a NoteColour.
  _ => null,
};

DateTime? _timestamp(Object? usec) {
  if (usec is! int) return null;
  try {
    return DateTime.fromMicrosecondsSinceEpoch(usec, isUtc: true);
  } on ArgumentError {
    // A value outside DateTime's range is a broken record, not a reason to
    // lose the whole export.
    return null;
  }
}
