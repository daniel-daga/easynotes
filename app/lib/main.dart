/// A device's notes, backed by the Rust core, synced through a relay.
library;

import 'dart:async';

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'core/browser_store.dart';
import 'core/master_key.dart';
import 'core/note_library.dart';
import 'core/reminder_scheduler.dart';
import 'core/rust_note_core.dart';
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
  if (kIsWeb) return openInBrowser();
  final directory = (await getApplicationSupportDirectory()).path;
  // The bridge up front. Not load-bearing any more — every entry point below
  // brings it up itself, including [MasterKeyStore.forDirectory], which is the
  // one that crashed a real device's first launch by crossing the bridge before
  // anything had opened it. Kept because this is where the app would rather
  // find out that the native library is missing than three calls later.
  await RustNoteCore.ensureInitialized();
  // The keychain first, because it is what says whether this device has a key
  // at all — and because a device that updated into this version has one in a
  // file that has to be adopted rather than replaced.
  final masterKey = await MasterKeyStore().forDirectory(directory);
  final library = await RustNoteLibrary.open(directory, masterKey);
  // Connecting cannot fail loudly enough to matter: with no relay the app is a
  // local notes app, which is what it is designed to be when the network is
  // gone.
  if (relayUrl.isNotEmpty) await library.connect(relayUrl);
  return library;
}

/// Open this browser's notes out of IndexedDB, and start syncing them.
///
/// A tab is a device like any other now: it keeps what it holds and it reaches
/// the relay, over the browser's own WebSocket rather than a TCP socket it
/// cannot open. What is still true of a tab and not of a phone is written down
/// in [ADR 0011](../../docs/adr/0011-the-web-store.md).
///
/// A browser that will not give the app a database is not an error worth
/// refusing to start over: some private-browsing modes do not, and it can be
/// switched off outright. The notes go in memory for that tab, which is what
/// every tab had before this, and the app runs.
Future<NoteLibrary> openInBrowser() async {
  final masterKey = await MasterKeyStore().forBrowser();
  NoteLibrary library;
  try {
    library = await RustNoteLibrary.openInBrowser(
      await openBrowserStore(browserDatabase),
      masterKey,
    );
  } on Object catch (error) {
    debugPrint('keeping this tab\'s notes in memory only: $error');
    library = await RustNoteLibrary.openEphemeral();
  }
  // As on a device: with no relay there the app is a local notes app, and the
  // status button says so.
  if (relayUrl.isNotEmpty) await library.connect(relayUrl);
  return library;
}

/// The name of the IndexedDB database. One per origin, so it says what it is
/// rather than which notes are in it.
const browserDatabase = 'easynotes';

class EasyNotesApp extends StatefulWidget {
  const EasyNotesApp({super.key, required this.library});

  final NoteLibrary library;

  @override
  State<EasyNotesApp> createState() => _EasyNotesAppState();
}

class _EasyNotesAppState extends State<EasyNotesApp> {
  /// Sends held typing the moment the app stops being seen — backgrounded on a
  /// phone, minimised on a desktop, a tab switched away from in a browser. All
  /// three are `hidden` to Flutter, and it is the last state the app is sure
  /// to reach with its sockets still up.
  late final AppLifecycleListener _lifecycle;
  late final ReminderScheduler _reminders;

  @override
  void initState() {
    super.initState();
    _reminders = ReminderScheduler(widget.library);
    unawaited(_reminders.start());
    _lifecycle = AppLifecycleListener(
      onHide: widget.library.sendHeldEdits,
      onResume: () => unawaited(_reminders.reconcile()),
    );
  }

  @override
  void dispose() {
    _lifecycle.dispose();
    unawaited(_reminders.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final library = widget.library;
    return MaterialApp(
      title: 'EasyNotes',
      debugShowCheckedModeBanner: false,
      // `fontFamily` names the Roboto bundled in `pubspec.yaml` rather than
      // the one the web engine would fetch from fonts.gstatic.com. Saying it
      // here is what makes the bundled copy the font that is actually drawn on
      // every platform, instead of 130 KB of assets that only the web build
      // has a use for.
      theme: ThemeData(
        colorSchemeSeed: Colors.amber,
        useMaterial3: true,
        brightness: Brightness.light,
        fontFamily: 'Roboto',
      ),
      darkTheme: ThemeData(
        colorSchemeSeed: Colors.amber,
        useMaterial3: true,
        brightness: Brightness.dark,
        fontFamily: 'Roboto',
      ),
      home: NoteListScreen(library: library),
    );
  }
}
