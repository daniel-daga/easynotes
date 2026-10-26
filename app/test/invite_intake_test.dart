/// What happens when a link arrives at the app rather than at a screen.
///
/// The bug this covers: a link naming a sync server the device was not on made
/// the guest add the server first, and the rebuild that followed threw the
/// invite away — so the same link had to be pasted a second time. Now the app
/// holds the link, moves the device itself, and hands the link on.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/relay_session.dart';
import 'package:easynotes/main.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

const _secret = 'ASRciIVJtU8m60KIfMaTLg1HrJI9dhPOzsPW5gQdPigp';
const _access = 'Zm9vYmFyLXJlbGF5LWtleQ';
const _relay = 'https://sync.example.com';
const _params = 'relay=$_relay;access=$_access';
const _link = 'easynotes://join#$_params#$_secret';

void main() {
  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
  });

  /// The app over fakes. [existingRelay] is the server this device is already
  /// on, if it is on one.
  Future<(RelaySession, List<FakeNoteLibrary>)> pumpApp(
    WidgetTester tester, {
    String? existingRelay,
    String? link,
    bool onboarding = false,
    bool account = true,
  }) async {
    final profiles = RelayProfiles();
    await profiles.load();
    if (existingRelay != null) {
      await profiles.activate(await profiles.prepare(existingRelay));
    }
    FakeNoteLibrary library() {
      final next = FakeNoteLibrary();
      if (account) next.setUpSharing('Anna');
      return next;
    }

    final opened = <FakeNoteLibrary>[];
    final session = RelaySession(
      profiles: profiles,
      library: library(),
      profile: profiles.selected,
      open: (profile, {connect = true, allowEphemeral = true}) async {
        final next = library();
        opened.add(next);
        return next;
      },
    );
    await tester.pumpWidget(
      EasyNotesApp(
        session: session,
        initialInviteLink: link,
        showOnboarding: onboarding,
      ),
    );
    // The intake runs off a frame callback and then awaits a profile switch,
    // which is not a frame: give the futures room to finish. Not
    // `pumpAndSettle`, because a switch draws a spinner while it happens.
    for (var i = 0; i < 20; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    return (session, opened);
  }

  testWidgets('a link naming another server moves the device, then joins', (
    tester,
  ) async {
    final (session, opened) = await pumpApp(tester, link: _link);

    // A device with no server of its own has nothing to lose, so there is
    // nothing to ask: it is on the link's server and asking to join, once.
    expect(session.profile.url, _relay);
    expect(find.byKey(const Key('join-confirm')), findsOneWidget);
    expect(find.byKey(const Key('join-explainer')), findsOneWidget);

    await tester.tap(find.byKey(const Key('join-confirm')));
    // The waiting dialog animates for as long as it is up.
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    expect(
      opened.single.calls,
      contains('requestJoin(easynotes://join#$_params#$_secret)'),
    );
  });

  testWidgets('a device already on a server is asked before it moves', (
    tester,
  ) async {
    final (session, opened) = await pumpApp(
      tester,
      existingRelay: 'https://mine.example',
      link: _link,
    );

    // Nothing moved, and nothing joined, until the question is answered.
    expect(find.text('This note is kept somewhere else'), findsOneWidget);
    expect(
      find.textContaining(RegExp('server|relay', caseSensitive: false)),
      findsNothing,
    );
    await tester.tap(find.byKey(const Key('join-switch-cancel')));
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    expect(session.profile.url, 'https://mine.example');
    expect(find.byKey(const Key('join-confirm')), findsNothing);
    expect(opened, isEmpty);
  });

  testWidgets('a first-time joiner gets the join account copy', (tester) async {
    await pumpApp(tester, link: _link, account: false);

    expect(find.text('One step before you join'), findsOneWidget);
    expect(find.textContaining('needs twelve words'), findsOneWidget);
    expect(
      find.textContaining(
        RegExp('server|access key|encrypt|ciphertext', caseSensitive: false),
      ),
      findsNothing,
    );
  });

  testWidgets('a link that arrived during onboarding waits for it', (
    tester,
  ) async {
    await pumpApp(tester, link: _link, onboarding: true);

    // Onboarding first: a link is not a reason to throw away the first run.
    expect(find.byKey(const Key('onboarding-welcome')), findsOneWidget);
    expect(find.byKey(const Key('join-confirm')), findsNothing);

    await tester.ensureVisible(find.byKey(const Key('onboarding-next')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('onboarding-next')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('onboarding-continue')));
    await tester.pumpAndSettle();
    await tester.ensureVisible(
      find.byKey(const Key('onboarding-start-writing')),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('onboarding-start-writing')));
    await tester.pumpAndSettle();

    // And then the link is still in hand.
    expect(find.byKey(const Key('join-confirm')), findsOneWidget);
  });
}
