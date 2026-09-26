/// A device's notes, backed by the Rust core, synced through a relay.
library;

import 'dart:async';

import 'package:flutter/foundation.dart'
    show defaultTargetPlatform, kIsWeb, TargetPlatform;
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'core/address_bar_stub.dart'
    if (dart.library.js_interop) 'core/address_bar.dart'
    as address_bar;
import 'core/browser_store.dart';
import 'core/deep_links.dart';
import 'core/invite_links.dart';
import 'core/local_notifications.dart';
import 'core/master_key.dart';
import 'core/note_library.dart';
import 'core/reminder_scheduler.dart';
import 'core/relay_profiles.dart';
import 'core/rust_note_core.dart';
import 'core/rust_note_library.dart';
import 'core/shared_text.dart';
import 'ui/invites.dart' show joinAlerts;
import 'ui/note_list_screen.dart';

ThemeData _easyNotesTheme(Brightness brightness) {
  final scheme = ColorScheme.fromSeed(
    seedColor: const Color(0xFFFFB300),
    brightness: brightness,
    dynamicSchemeVariant: DynamicSchemeVariant.tonalSpot,
  );
  return ThemeData(
    colorScheme: scheme,
    useMaterial3: true,
    brightness: brightness,
    fontFamily: 'Roboto',
    scaffoldBackgroundColor: scheme.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      surfaceTintColor: Colors.transparent,
      scrolledUnderElevation: 1,
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      surfaceTintColor: Colors.transparent,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(16),
        side: BorderSide(color: scheme.outlineVariant),
      ),
    ),
    dialogTheme: DialogThemeData(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(28)),
    ),
    bottomSheetTheme: const BottomSheetThemeData(
      showDragHandle: true,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(28)),
      ),
    ),
    floatingActionButtonTheme: FloatingActionButtonThemeData(
      elevation: 3,
      focusElevation: 3,
      hoverElevation: 4,
      highlightElevation: 5,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(18)),
    ),
  );
}

bool get _hasNativeDeepLinks =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS);

/// Where another app's share sheet can hand text in. Android only among the
/// native platforms: iOS needs a share extension, which is not built yet.
bool get _hasNativeShares =>
    !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final initialInvite = kIsWeb
      ? inviteLinkFrom(Uri.base.toString())
      : _hasNativeDeepLinks
      ? await DeepLinks.initialInvite()
      : null;
  final initialShare = kIsWeb
      ? sharedTextFromUri(Uri.base)
      : _hasNativeShares
      ? await SharedTexts.initial()
      : null;
  // Read, so gone from the address: a reload must not make the note twice.
  if (kIsWeb) address_bar.dropQuery();
  final profiles = RelayProfiles();
  await profiles.load();
  if (kIsWeb) {
    runApp(
      const MaterialApp(
        home: Scaffold(
          body: Center(
            child: Text(
              'Opening your notes…\nIf EasyNotes is open in another tab, close it to continue here.',
              textAlign: TextAlign.center,
            ),
          ),
        ),
      ),
    );
  }
  runApp(
    EasyNotesApp(
      library: await openLibrary(profiles.selected),
      profiles: profiles,
      profile: profiles.selected,
      initialInviteLink: initialInvite,
      initialSharedText: initialShare,
    ),
  );
}

/// Open this device's notes, and start syncing them.
Future<NoteLibrary> openLibrary(
  RelayProfile profile, {
  bool connect = true,
  bool allowEphemeral = true,
}) async {
  if (kIsWeb) {
    return openInBrowser(
      profile,
      connect: connect,
      allowEphemeral: allowEphemeral,
    );
  }
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
  final profileDirectory = '$directory/easynotes-${profile.id}';
  final masterKey = await MasterKeyStore().forDirectory(
    profileDirectory,
    profileId: profile.id,
  );
  final library = await RustNoteLibrary.open(profileDirectory, masterKey);
  // Connecting cannot fail loudly enough to matter: with no relay the app is a
  // local notes app, which is what it is designed to be when the network is
  // gone.
  if (connect && profile.url != null) await library.connect(profile.url!);
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
Future<NoteLibrary> openInBrowser(
  RelayProfile profile, {
  bool connect = true,
  bool allowEphemeral = true,
}) async {
  NoteLibrary library;
  BrowserStore? db;
  try {
    // Own the database before reading or minting the shared master key too.
    db = await openBrowserStore('$browserDatabase-${profile.id}');
    final masterKey = await MasterKeyStore().forBrowser(profileId: profile.id);
    library = await RustNoteLibrary.openInBrowser(db, masterKey);
  } on Object catch (error) {
    await db?.close();
    if (!allowEphemeral) rethrow;
    debugPrint('keeping this tab\'s notes in memory only: $error');
    library = await RustNoteLibrary.openEphemeral();
  }
  // As on a device: with no relay there the app is a local notes app, and the
  // status button says so.
  if (connect && profile.url != null) await library.connect(profile.url!);
  return library;
}

/// Base name for this origin's profile-specific IndexedDB databases.
const browserDatabase = 'easynotes';

class EasyNotesApp extends StatefulWidget {
  const EasyNotesApp({
    super.key,
    required this.library,
    required this.profiles,
    required this.profile,
    this.initialInviteLink,
    this.initialSharedText,
  });

  final NoteLibrary library;
  final RelayProfiles profiles;
  final RelayProfile profile;
  final String? initialInviteLink;
  final SharedText? initialSharedText;

  @override
  State<EasyNotesApp> createState() => _EasyNotesAppState();
}

class _EasyNotesAppState extends State<EasyNotesApp> {
  /// Sends held typing the moment the app stops being seen — backgrounded on a
  /// phone, minimised on a desktop, a tab switched away from in a browser. All
  /// three are `hidden` to Flutter, and it is the last state the app is sure
  /// to reach with its sockets still up.
  AppLifecycleListener? _lifecycle;
  late ReminderScheduler _reminders;
  late NoteLibrary _library;
  late RelayProfile _profile;
  bool _switching = false;
  bool _launchPayloadsDelivered = false;

  @override
  void initState() {
    super.initState();
    _library = widget.library;
    _profile = widget.profile;
    _installSession();
  }

  void _installSession() {
    _reminders = ReminderScheduler(_library);
    unawaited(_reminders.start());
    joinAlerts = SystemJoinAlerts(_library);
    _lifecycle = AppLifecycleListener(
      onHide: _library.sendHeldEdits,
      onResume: () => unawaited(_reminders.reconcile()),
    );
  }

  Future<void> _switchRelay(String? url) async {
    if (_switching || url == _profile.url) return;
    setState(() => _switching = true);
    final previousProfile = _profile;
    NoteLibrary? next;
    var oldDisposed = false;
    try {
      final target = await widget.profiles.prepare(url);
      next = await openLibrary(
        target,
        connect: false,
        allowEphemeral: false,
      );
      if (_library is RustNoteLibrary &&
          !(_library as RustNoteLibrary).canJoin) {
        throw StateError(
          'This tab has no persistent storage. Keep it open and switch on a tab with storage enabled.',
        );
      }
      if (_library is RustNoteLibrary) {
        await (_library as RustNoteLibrary).flush();
      }
      if (_library.storageError != null) {
        await next.dispose();
        next = null;
        throw StateError('This profile has unsaved local changes.');
      }
      final old = _library;
      _lifecycle?.dispose();
      await _reminders.dispose(cancelScheduled: true);
      final oldJoinAlerts = joinAlerts;
      if (oldJoinAlerts is SystemJoinAlerts) {
        await oldJoinAlerts.dispose(cancelScheduled: true);
      }
      await old.dispose();
      oldDisposed = true;
      await widget.profiles.activate(target);
      final opened = next!;
      if (target.url != null) await opened.connect(target.url!);
      if (!mounted) {
        await opened.dispose();
        return;
      }
      setState(() {
        _library = opened;
        _profile = target;
      });
      _installSession();
      next = null;
    } on Object catch (error) {
      if (next != null) await next.dispose();
      if (oldDisposed && mounted) {
        try {
          if (next == null) await _library.dispose();
          await widget.profiles.activate(previousProfile);
          final restored = await openLibrary(previousProfile);
          setState(() {
            _library = restored;
            _profile = previousProfile;
          });
          _installSession();
        } on Object catch (restoreError) {
          debugPrint(
            'could not reopen the previous relay profile: $restoreError',
          );
        }
      }
      if (mounted) {
        setState(() => _switching = false);
      }
      Error.throwWithStackTrace(error, StackTrace.current);
    } finally {
      if (mounted) setState(() => _switching = false);
    }
  }

  @override
  void dispose() {
    _lifecycle?.dispose();
    unawaited(_reminders.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final library = _library;
    return MaterialApp(
      title: 'EasyNotes',
      debugShowCheckedModeBanner: false,
      // `fontFamily` names the Roboto bundled in `pubspec.yaml` rather than
      // the one the web engine would fetch from fonts.gstatic.com. Saying it
      // here is what makes the bundled copy the font that is actually drawn on
      // every platform, instead of 130 KB of assets that only the web build
      // has a use for.
      theme: _easyNotesTheme(Brightness.light),
      darkTheme: _easyNotesTheme(Brightness.dark),
      home: _noteList(library),
    );
  }

  Widget _noteList(NoteLibrary library) {
    final initialInvite =
        _launchPayloadsDelivered ? null : widget.initialInviteLink;
    final initialShare =
        _launchPayloadsDelivered ? null : widget.initialSharedText;
    _launchPayloadsDelivered = true;
    return NoteListScreen(
      key: ValueKey(_profile.id),
      library: library,
      profile: _profile,
      profiles: widget.profiles,
      onSwitchRelay: _switchRelay,
      initialInviteLink: initialInvite,
      inviteLinks: _hasNativeDeepLinks ? DeepLinks.invites : null,
      initialSharedText: initialShare,
      sharedTexts: _hasNativeShares ? SharedTexts.incoming : null,
    );
  }
}
