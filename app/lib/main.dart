/// Slice 1's app: one checklist, backed by the Rust core.
library;

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'core/note_core.dart';
import 'core/rust_note_core.dart';
import 'ui/checklist_screen.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(EasyNotesApp(core: await openCore()));
}

/// Open the note this slice edits.
///
/// Slice 1 has exactly one note and no note list — the plan's out-of-scope list
/// is explicit about that. Slice 2 replaces this with the real local store and a
/// grid of notes.
Future<NoteCore> openCore() async {
  if (kIsWeb) {
    // No filesystem in a browser, and the op log's IndexedDB backing is unfinished
    // M4 work, so the web build keeps the note in memory for now. The list is
    // gone when the tab closes; ADR 0002 records what that costs and what is left.
    return RustNoteCore.openEphemeral();
  }
  final dir = await getApplicationSupportDirectory();
  return RustNoteCore.open(dir.path);
}

class EasyNotesApp extends StatelessWidget {
  const EasyNotesApp({super.key, required this.core});

  final NoteCore core;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'EasyNotes',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorSchemeSeed: Colors.amber,
        useMaterial3: true,
        brightness: Brightness.light,
      ),
      darkTheme: ThemeData(
        colorSchemeSeed: Colors.amber,
        useMaterial3: true,
        brightness: Brightness.dark,
      ),
      home: ChecklistScreen(core: core),
    );
  }
}
