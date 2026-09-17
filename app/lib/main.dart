/// One checklist, backed by the Rust core, synced through a relay.
library;

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'core/note_core.dart';
import 'core/rust_note_core.dart';
import 'ui/checklist_screen.dart';

/// Where the relay is.
///
/// A compile-time setting rather than a screen, because Slice 2 has no settings
/// and a hard-coded address that can be overridden is honest about that:
///
///     flutter run --dart-define=EASYNOTES_RELAY=https://relay.example
///
/// The default is the one `just run-relay` starts. Nothing is lost when it is
/// not there: the app works offline and the status button says so.
const relayUrl = String.fromEnvironment(
  'EASYNOTES_RELAY',
  defaultValue: 'http://127.0.0.1:8080',
);

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(
    EasyNotesApp(
      core: await openCore(),
      join: kIsWeb ? null : joinCore,
    ),
  );
}

/// Open the note this slice edits, and start syncing it.
///
/// Slice 1 had exactly one note and no note list; Slice 2 keeps that and adds
/// the relay. The real local store and a grid of notes are still ahead.
Future<NoteCore> openCore() async {
  if (kIsWeb) {
    // No filesystem in a browser, and no relay transport either: both are the
    // unfinished half of M4. ADR 0002 records what that costs. The app still
    // runs the same Rust core — it just cannot keep or share what it holds.
    return RustNoteCore.openEphemeral();
  }
  final core = await RustNoteCore.open((await getApplicationSupportDirectory()).path);
  await _startSyncing(core);
  return core;
}

/// Join the list a share blob describes, and sync that one instead.
Future<NoteCore> joinCore(String blob) async {
  final core = await RustNoteCore.join(
    (await getApplicationSupportDirectory()).path,
    blob,
  );
  await _startSyncing(core);
  return core;
}

/// Connecting cannot fail loudly enough to matter: with no relay the app is a
/// local notes app, which is what it is designed to be when the network is gone.
Future<void> _startSyncing(NoteCore core) async {
  if (relayUrl.isEmpty) return;
  await core.connect(relayUrl);
}

class EasyNotesApp extends StatefulWidget {
  const EasyNotesApp({super.key, required this.core, this.join});

  final NoteCore core;

  /// How to join a shared list. Null on the web, which cannot yet: joining means
  /// holding the note's key and its log, and the browser build keeps neither
  /// past the tab.
  final Future<NoteCore> Function(String blob)? join;

  @override
  State<EasyNotesApp> createState() => _EasyNotesAppState();
}

class _EasyNotesAppState extends State<EasyNotesApp> {
  late NoteCore _core = widget.core;

  /// Swap the note this device holds for the shared one.
  ///
  /// The old core is disposed *after* the new one is open, so a blob that turns
  /// out to be unreadable leaves the app with the list it already had rather
  /// than with nothing.
  Future<void> _join(String blob) async {
    final joining = widget.join;
    if (joining == null) return;
    final replacement = await joining(blob);
    final previous = _core;
    setState(() => _core = replacement);
    await previous.dispose();
  }

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
      home: ChecklistScreen(
        // Keyed on the core so that joining a list rebuilds the screen from the
        // new one instead of leaving the old note's stream attached.
        key: ValueKey(identityHashCode(_core)),
        core: _core,
        onJoin: widget.join == null ? null : _join,
      ),
    );
  }
}
