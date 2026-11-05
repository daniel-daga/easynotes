/// Reading an extracted Google Keep Takeout export.
///
/// This is the parsing half of an import: it turns the text of
/// `Takeout/Keep/*.json` into plain values for the UI and never creates a note.
/// Unpacking the zip is the caller's job; this takes the extracted files.
library;

import 'dart:convert';

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
