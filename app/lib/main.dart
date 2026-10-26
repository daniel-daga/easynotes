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
  // As on a device: a profile is a server and the account that belongs to it
  // (ADR 0031). Without the account there is nothing to sync, so a profile
  // left half set up by an interrupted run stays local until it is finished.
  if (connect && profile.url != null && library.hasAccount) {
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

  /// A link that arrived and has not been joined or turned down yet. It lives
  /// here rather than on the list because joining can replace the list's whole
  /// profile; this holder does not go away when it does.
  String? _pendingInvite;
  StreamSubscription<String>? _inviteSubscription;
  bool _initialShareDelivered = false;
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
    // A link tapped while the app runs is handled here rather than on the
    // list, so that one which arrives during onboarding waits instead of
    // being lost.
    if (_hasNativeDeepLinks) {
      _inviteSubscription = DeepLinks.invites.listen(_takeInvite);
    }
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final link = widget.initialInviteLink;
      if (link != null) unawaited(_takeInvite(link));
    });
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
    // "Continue offline" may be the way out of a setup that saved a server
    // but never finished its account. A server without an account is not a
    // state to leave behind (ADR 0031), so end that relationship first.
    if (_session.profile.url != null && !_session.library.hasAccount) {
      await _session.reopenAsLocal();
    }
    await _session.profiles.completeOnboarding();
    if (mounted) setState(() => _showOnboarding = false);
  }

  /// The account half of sync setup, over the library that is about to be
  /// bound to [profile]'s server. Returns whether an account now exists.
  Future<bool> _setUpAccount(
    NoteLibrary library,
    RelayProfile profile, {
    bool joining = false,
  }) async {
    final navigator = _navigatorKey.currentState;
    if (navigator == null) return false;
    await navigator.push<void>(
      MaterialPageRoute<void>(
        builder: (_) => AccountScreen(
          library: library,
          profile: profile,
          creating: true,
          joining: joining,
        ),
      ),
    );
    return library.hasAccount;
  }

  Future<void> _connectNewServer(
    String address, {
    String? access,
    required bool moveNotes,
    bool joining = false,
  }) => _session.connectNewServer(
    address,
    access: access,
    moveNotes: moveNotes,
    setUpAccount: (library, profile) =>
        _setUpAccount(library, profile, joining: joining),
  );

  Future<void> _setUpSync() async {
    final profile = _session.profile;
    // A setup interrupted after its server was saved: finish the account on
    // the profile already selected, then start syncing. Asking for the server
    // again would be a second setup for the same one.
    if (profile.url != null && !_session.library.hasAccount) {
      final ready = await _setUpAccount(_session.library, profile);
      if (!mounted || !ready) return;
      try {
        await _session.connectCurrent();
      } on Object {
        // The status card says why sync is not running; the setup itself is
        // complete and stays that way.
      }
      if (!mounted) return;
      setState(() {
        _relayConnectedDuringOnboarding = true;
        _accountCreatedDuringOnboarding = true;
      });
      return;
    }

    final setUp = await _navigatorKey.currentState!.push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => RelaySetupWizard(
          onSwitchRelay: (url, {access}) async {
            if (url == null) return _session.switchTo(null);
            await _connectNewServer(url, access: access, moveNotes: false);
          },
          onMoveLocalToRelay: (url, {access}) =>
              _connectNewServer(url, access: access, moveNotes: true),
          onVerifyRelay: _session.library.verifyRelay,
          profile: _session.profile,
          profiles: _session.profiles.profiles,
          hasLocalNotes: _session.library.notes.isNotEmpty,
        ),
      ),
    );
    if (!mounted || setUp != true) return;
    setState(() {
      _relayConnectedDuringOnboarding = true;
      _accountCreatedDuringOnboarding = true;
    });
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
    // Cleanup, not a step of the switch. A plugin that is not ready yet — a
    // link tapped a second after launch, while the reminders are still
    // starting — must not stop this device moving to the note's relay; what
    // it costs is that reminders already posted stay posted.
    try {
      await _reminders?.dispose(cancelScheduled: true);
    } on Object catch (error) {
      debugPrint('could not stop reminders: $error');
    }
    _reminders = null;
    final alerts = joinAlerts;
    if (alerts is SystemJoinAlerts) {
      try {
        await alerts.dispose(cancelScheduled: true);
      } on Object catch (error) {
        debugPrint('could not stop join alerts: $error');
      }
    }
  }

  @override
  void dispose() {
    _session
      ..removeListener(_changed)
      ..onRelease = null
      ..onInstall = null;
    unawaited(_inviteSubscription?.cancel());
    _lifecycle?.dispose();
    unawaited(_reminders?.dispose());
    super.dispose();
  }

  /// A link this app was given: tapped, opened from a browser, or pasted into
  /// the drawer.
  ///
  /// If it names a sync server this device is not on, that is settled first —
  /// with one question when this device already has notes somewhere else —
  /// because the join request has to be posted to the server that holds the
  /// note. Only then does the list see the link, so nothing typed is lost on
  /// the way and nothing is asked twice.
  Future<void> _takeInvite(String link) async {
    if (!mounted) return;
    final relay = relayFromInvite(link);
    debugPrint(
      'join: link names ${relay?.url} (this device: ${_session.profile.url})',
    );
    if (relay != null && !await _settleRelay(relay)) return;
    if (!mounted) return;
    // Cleared first, so that the same link delivered twice still shows: the
    // list watches this change, not the value.
    setState(() => _pendingInvite = null);
    await WidgetsBinding.instance.endOfFrame;
    if (!mounted) return;
    setState(() => _pendingInvite = link);
  }

  /// Put this device on [relay]. `false` when the person said no, or when the
  /// switch failed and there is nothing to join from.
  Future<bool> _settleRelay(InviteRelay relay) async {
    final String target;
    try {
      target = canonicalRelayUrl(relay.url);
    } on FormatException {
      // Not a server this build can use. The join dialog is where that is
      // explained, with the link still in hand.
      return true;
    }
    final current = _session.profile.url;
    if (target == current) return true;
    // A device with no server yet has nothing to lose, and asks nothing.
    if (current != null && !await _confirmRelaySwitch()) return false;
    try {
      // Joining needs the server *and* the account that belongs to it
      // (ADR 0031). A new server is set up in one step here, before the
      // join request goes anywhere.
      await _connectNewServer(
        target,
        access: relay.access,
        moveNotes: false,
        joining: true,
      );
      return true;
    } on SyncSetupCancelled {
      return false;
    } on Object catch (error) {
      debugPrint('join: could not switch to $target: $error');
      _say('Could not open the note. Ask for a new link.');
      return false;
    }
  }

  Future<bool> _confirmRelaySwitch() async {
    final context = _navigatorKey.currentContext;
    if (context == null) return true;
    final go = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('This note is kept somewhere else'),
        content: const Text(
          'This note is shared through a different EasyNotes setup. Joining '
          'switches this app to it. Your own notes stay where they are and '
          'come back when you switch back.',
        ),
        actions: [
          TextButton(
            key: const Key('join-switch-cancel'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const Key('join-switch'),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Switch and join'),
          ),
        ],
      ),
    );
    return go ?? false;
  }

  void _say(String message) {
    final context = _navigatorKey.currentContext;
    if (context == null || !context.mounted) return;
    ScaffoldMessenger.maybeOf(
      context,
    )?.showSnackBar(SnackBar(content: Text(message)));
  }

  /// The list has joined or turned down [link]; it never needs showing again.
  void _inviteHandled(String link) {
    if (_pendingInvite == link) _pendingInvite = null;
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
              onSetUpSync: _setUpSync,
            )
          : _noteList(library),
    );
  }

  Widget _noteList(NoteLibrary library) {
    final initialShare = _initialShareDelivered
        ? null
        : widget.initialSharedText;
    _initialShareDelivered = true;
    return NoteListScreen(
      key: ValueKey('${_session.profile.id}/${_session.generation}'),
      library: library,
      profile: _session.profile,
      profiles: _session.profiles,
      onSwitchRelay: (url, {access}) async {
        if (url == null) return _session.switchTo(null);
        await _connectNewServer(url, access: access, moveNotes: false);
      },
      onMoveLocalToRelay: (url, {access}) =>
          _connectNewServer(url, access: access, moveNotes: true),
      onSetAccess: _session.setAccess,
      onAccountDeleted: _session.reopenAsLocal,
      initialInviteLink: _pendingInvite,
      onInviteLink: _takeInvite,
      onInviteHandled: _inviteHandled,
      initialSharedText: initialShare,
      sharedTexts: _hasNativeShares ? SharedTexts.incoming : null,
    );
  }
}
