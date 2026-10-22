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
import 'core/app_visibility.dart';
import 'core/browser_store.dart';
import 'core/deep_links.dart';
import 'core/invite_links.dart';
import 'core/local_notifications.dart';
import 'core/master_key.dart';
import 'core/note_library.dart';
import 'core/profile_library.dart';
import 'core/reminder_scheduler.dart';
import 'core/relay_profiles.dart';
import 'core/relay_session.dart';
import 'core/rust_note_library.dart';
import 'core/shared_text.dart';
import 'ui/account_screen.dart';
import 'ui/about_screen.dart' show registerBundledLicenses;
import 'ui/invites.dart' show joinAlerts;
import 'ui/note_list_screen.dart';
import 'ui/onboarding_screen.dart';
import 'ui/relay_setup_wizard.dart';

ThemeData _easyNotesTheme(Brightness brightness) {
  final light = brightness == Brightness.light;
  // A black or white seed gives neutral greys, but Material still puts the
  // accent roles at tone 40 and 80. Pin them to the seed itself — black on
  // light, white on dark — with containers one step off the surface, so that
  // a filled button or the FAB reads as ink on paper rather than grey.
  //
  // The monochrome variant is what makes the greys actually neutral: tonal
  // spot forces chroma 6 at the seed's hue, and an achromatic seed reports
  // hue 0 (red) for black, leaving the surfaces with a warm cast.
  final accent = light ? Colors.black : Colors.white;
  final onAccent = light ? Colors.white : Colors.black;
  final container = light ? const Color(0xFFEFEFEF) : const Color(0xFF262626);
  final scheme =
      ColorScheme.fromSeed(
        seedColor: accent,
        brightness: brightness,
        dynamicSchemeVariant: DynamicSchemeVariant.monochrome,
      ).copyWith(
        primary: accent,
        onPrimary: onAccent,
        primaryContainer: container,
        onPrimaryContainer: accent,
        secondary: accent,
        onSecondary: onAccent,
        secondaryContainer: container,
        onSecondaryContainer: accent,
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
      actionsPadding: const EdgeInsets.only(right: 8),
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
      backgroundColor: scheme.primary,
      foregroundColor: scheme.onPrimary,
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
  registerBundledLicenses();
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
  if (profiles.shouldShowOnboarding &&
      (initialInvite != null || initialShare != null)) {
    await profiles.completeOnboarding();
  }
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
      session: RelaySession(
        profiles: profiles,
        library: await openLibrary(profiles.selected),
        profile: profiles.selected,
        open: openLibrary,
      ),
      initialInviteLink: initialInvite,
      initialSharedText: initialShare,
      showOnboarding: profiles.shouldShowOnboarding,
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
  return openProfileLibrary(
    (await getApplicationSupportDirectory()).path,
    profile,
    connect: connect,
  );
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
  if (connect && profile.url != null) {
    await library.connect(profile.url!, access: profile.access);
  }
  return library;
}

/// Base name for this origin's profile-specific IndexedDB databases.
const browserDatabase = 'easynotes';

class EasyNotesApp extends StatefulWidget {
  const EasyNotesApp({
    super.key,
    required this.session,
    this.initialInviteLink,
    this.initialSharedText,
    this.showOnboarding = false,
  });

  final RelaySession session;
  final String? initialInviteLink;
  final SharedText? initialSharedText;
  final bool showOnboarding;

  @override
  State<EasyNotesApp> createState() => _EasyNotesAppState();
}

class _EasyNotesAppState extends State<EasyNotesApp> {
  final _navigatorKey = GlobalKey<NavigatorState>();

  /// Sends held typing the moment the app stops being seen — backgrounded on a
  /// phone, minimised on a desktop, a tab switched away from in a browser. All
  /// three are `hidden` to Flutter, and it is the last state the app is sure
  /// to reach with its sockets still up.
  AppLifecycleListener? _lifecycle;
  ReminderScheduler? _reminders;
  bool _launchPayloadsDelivered = false;
  late bool _showOnboarding = widget.showOnboarding;
  late bool _relayConnectedDuringOnboarding;
  late bool _accountCreatedDuringOnboarding;

  /// Between a library's release and its replacement's install, when nothing
  /// may draw from it: see [_releaseSession].
  bool _released = false;

  RelaySession get _session => widget.session;

  @override
  void initState() {
    super.initState();
    // Onboarding can be pending across a restart — the first run was closed
    // before its last page. Whatever was set up before is still set up, so
    // the checklist starts from the device rather than from empty.
    _relayConnectedDuringOnboarding = _session.profile.url != null;
    _accountCreatedDuringOnboarding = _hasAccount(_session.library);
    _session
      ..onRelease = _releaseSession
      ..onInstall = _installSession
      ..addListener(_changed);
    _installSession(_session.library);
  }

  static bool _hasAccount(NoteLibrary library) {
    try {
      return library.hasAccount;
    } on Object {
      return false;
    }
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  Future<void> _finishOnboarding() async {
    await _session.profiles.completeOnboarding();
    if (mounted) setState(() => _showOnboarding = false);
  }

  Future<void> _createOnboardingAccount() async {
    await _navigatorKey.currentState!.push<void>(
      MaterialPageRoute<void>(
        builder: (_) => AccountScreen(
          library: _session.library,
          profile: _session.profile,
          creating: true,
        ),
      ),
    );
    if (!mounted) return;
    // The screen closes either way; only an account that now exists turns the
    // checklist's second step into a tick and moves onboarding on to its last
    // page.
    if (_session.library.hasAccount) {
      setState(() => _accountCreatedDuringOnboarding = true);
    }
  }

  Future<void> _setUpRelay() async {
    final connected = await _navigatorKey.currentState!.push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => RelaySetupWizard(
          onSwitchRelay: _session.switchTo,
          onVerifyRelay: _session.library.verifyRelay,
        ),
      ),
    );
    if (!mounted || connected != true) return;
    setState(() => _relayConnectedDuringOnboarding = true);
  }

  void _installSession(NoteLibrary library) {
    // The session says it has changed once this returns, which is what
    // draws the note list again.
    _released = false;
    // Anything a failed release left behind goes first, so that a library
    // handed back after one never has two of each.
    _lifecycle?.dispose();
    unawaited(_reminders?.dispose());
    final reminders = ReminderScheduler(library);
    _reminders = reminders;
    unawaited(reminders.start());
    joinAlerts = SystemJoinAlerts(library);
    _lifecycle = AppLifecycleListener(
      onHide: () {
        appIsForeground.value = false;
        unawaited(library.setForeground(false));
      },
      // Also how a grant of exact alarms takes effect: it is made on a system
      // settings page, and coming back is the only sign of it.
      onResume: () {
        appIsForeground.value = true;
        unawaited(library.setForeground(true));
        unawaited(reminders.reconcile());
      },
    );
  }

  /// Nothing of the outgoing profile may fire once another is showing: its
  /// reminders and join alerts would name notes the new profile does not have.
  ///
  /// Nor may its note list draw. Closing the library ends its streams, and a
  /// list rebuilt on that reads the library it is built on, which is closed
  /// by then. Marking this widget dirty first means the next frame builds it
  /// before the list, and takes the list away; no frame is waited for, since
  /// a tab nobody is looking at draws none, and builds nothing either.
  Future<void> _releaseSession(NoteLibrary library) async {
    if (mounted) setState(() => _released = true);
    _lifecycle?.dispose();
    _lifecycle = null;
    await _reminders?.dispose(cancelScheduled: true);
    _reminders = null;
    final alerts = joinAlerts;
    if (alerts is SystemJoinAlerts) await alerts.dispose(cancelScheduled: true);
  }

  @override
  void dispose() {
    _session
      ..removeListener(_changed)
      ..onRelease = null
      ..onInstall = null;
    _lifecycle?.dispose();
    unawaited(_reminders?.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final library = _session.library;
    return MaterialApp(
      navigatorKey: _navigatorKey,
      title: 'EasyNotes',
      debugShowCheckedModeBanner: false,
      // `fontFamily` names the Roboto bundled in `pubspec.yaml` rather than
      // the one the web engine would fetch from fonts.gstatic.com. Saying it
      // here is what makes the bundled copy the font that is actually drawn on
      // every platform, instead of 130 KB of assets that only the web build
      // has a use for.
      theme: _easyNotesTheme(Brightness.light),
      darkTheme: _easyNotesTheme(Brightness.dark),
      // A library being swapped underneath onboarding is no reason to replace
      // the screen: onboarding draws nothing from it, and replacing it would
      // throw away the step a person is on — after connecting a sync server
      // they would be back at the intro.
      home: _released && !_showOnboarding
          ? const Scaffold(body: Center(child: CircularProgressIndicator()))
          : _showOnboarding
          ? OnboardingScreen(
              relayConnected: _relayConnectedDuringOnboarding,
              accountCreated: _accountCreatedDuringOnboarding,
              serverUrl: _session.profile.url,
              onContinue: _finishOnboarding,
              onSetUpRelay: _setUpRelay,
              onCreateAccount: _createOnboardingAccount,
            )
          : _noteList(library),
    );
  }

  Widget _noteList(NoteLibrary library) {
    final initialInvite = _launchPayloadsDelivered
        ? null
        : widget.initialInviteLink;
    final initialShare = _launchPayloadsDelivered
        ? null
        : widget.initialSharedText;
    _launchPayloadsDelivered = true;
    return NoteListScreen(
      key: ValueKey('${_session.profile.id}/${_session.generation}'),
      library: library,
      profile: _session.profile,
      profiles: _session.profiles,
      onSwitchRelay: _session.switchTo,
      onMoveLocalToRelay: _session.moveLocalToRelay,
      onSetAccess: _session.setAccess,
      onAccountDeleted: _session.reopen,
      initialInviteLink: initialInvite,
      inviteLinks: _hasNativeDeepLinks ? DeepLinks.invites : null,
      initialSharedText: initialShare,
      sharedTexts: _hasNativeShares ? SharedTexts.incoming : null,
    );
  }
}
