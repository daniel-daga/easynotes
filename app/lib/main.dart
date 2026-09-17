/// A device's notes, backed by the Rust core, synced through a relay.
library;

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'core/note_library.dart';
import 'core/rust_note_library.dart';
import 'ui/note_list_screen.dart';

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
  runApp(EasyNotesApp(library: await openLibrary()));
}

/// Open this device's notes, and start syncing them.
Future<NoteLibrary> openLibrary() async {
  if (kIsWeb) {
    // No filesystem in a browser, and no relay transport either: both are the
    // unfinished half of the web build. ADR 0002 records what that costs. The
    // app still runs the same Rust core — it just cannot keep or share what it
    // holds.
    return RustNoteLibrary.openEphemeral();
  }
  final library = await RustNoteLibrary.open(
    (await getApplicationSupportDirectory()).path,
  );
  // Connecting cannot fail loudly enough to matter: with no relay the app is a
  // local notes app, which is what it is designed to be when the network is
  // gone.
  if (relayUrl.isNotEmpty) await library.connect(relayUrl);
  return library;
}

class EasyNotesApp extends StatelessWidget {
  const EasyNotesApp({super.key, required this.library});

  final NoteLibrary library;

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
      home: NoteListScreen(library: library),
    );
  }
}
