/// Adding a device from an approved QR pairing, against fakes.
///
/// This holds the order the screen used to get wrong: on a device with no
/// account, moving the collection first asked the person to create an account,
/// and the adopted account was then refused. Adopting first is what lets the
/// move and the connection happen at all.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/invites.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/relay_session.dart';
import 'package:easynotes/ui/account_transfer_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

const server = 'https://relay.example';

/// A paired device, with the approved transfer waiting to be adopted.
///
/// [adoptAccountTransfer] refuses a second account and clears the pending
/// transfer, as the core does, so a retry that adopts twice fails here rather
/// than passing.
class _TransferLibrary extends FakeNoteLibrary implements AccountTransfers {
  ({String relayUrl, String? relayAccess})? offered = (
    relayUrl: server,
    relayAccess: null,
  );

  /// Fails the connection after the notes have been moved, as a relay that
  /// cannot be reached does; the fake's [connect] records the call first.
  bool refuseConnection = false;

  @override
  Future<void> connect(String url, {String? access}) async {
    await super.connect(url, access: access);
    if (refuseConnection) throw StateError('sync worker unavailable');
  }

  @override
  Future<void> adoptAccountTransfer() async {
    calls.add('adoptAccountTransfer()');
    final transfer = offered;
    if (transfer == null) throw 'No approved account transfer is ready.';
    if (hasAccount) throw 'this device already has an account';
    offered = null;
    setUpSharing();
  }

  @override
  Future<({String relayUrl, String? relayAccess})?>
  pollAccountTransferResult() async {
    calls.add('pollAccountTransferResult()');
    final transfer = offered;
    if (transfer == null) throw 'No device transfer is active.';
    return transfer;
  }

  @override
  Future<String> beginAccountTransferFromQr(String qr) async {
    calls.add('beginAccountTransferFromQr()');
    // A new pairing offers the same account and relay again, as the original
    // device's QR would.
    offered = (relayUrl: server, relayAccess: null);
    return '123456';
  }

  @override
  Future<String> beginAccountTransfer(
    String relayUrl,
    String? relayAccess,
  ) async => '{"version":1}';

  @override
  Future<String?> accountTransferRequestCode() async => null;

  @override
  Future<void> approveAccountTransfer() async {}

  @override
  Future<void> cancelAccountTransfer() async {}
}

/// Profiles whose move can be made to fail, as a keychain does.
class _Profiles extends RelayProfiles {
  bool failMove = false;

  @override
  Future<RelayProfile> moveLocalToRelay(
    String address, {
    String? access,
  }) async {
    if (failMove) throw StateError('keychain refused');
    return super.moveLocalToRelay(address, access: access);
  }
}

void main() {
  late _Profiles profiles;
  late _TransferLibrary library;
  late RelaySession session;
  late List<String> prompts;

  setUp(() async {
    FlutterSecureStorage.setMockInitialValues({});
    profiles = _Profiles();
    await profiles.load();
    library = _TransferLibrary();
    prompts = [];
    session = RelaySession(
      profiles: profiles,
      library: library,
      profile: profiles.selected,
      // A local device moving to its first relay keeps the collection it has.
      open: (_, {bool connect = true, bool allowEphemeral = true}) async =>
          throw StateError('no profile is opened during a transfer'),
    );
  });

  Future<void> openTransfer(WidgetTester tester) async {
    await tester.tap(find.byKey(const Key('open-transfer')));
    await tester.pumpAndSettle();
  }

  /// Open the transfer screen the way the settings screen does — pushed over
  /// something, so Cancel has somewhere to go back to.
  Future<void> pumpScreen(WidgetTester tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: Center(
              child: TextButton(
                key: const Key('open-transfer'),
                onPressed: () => Navigator.of(context).push<void>(
                  MaterialPageRoute<void>(
                    builder: (_) => AccountTransferScreen(
                      library: library,
                      profile: session.profile,
                      onMoveLocalToRelay: (url, {access}) =>
                          session.connectNewServer(
                            url,
                            access: access,
                            moveNotes: true,
                            setUpAccount: (library, profile) async {
                              prompts.add('account');
                              await library.createAccount();
                              return library.hasAccount;
                            },
                          ),
                      onSetAccess: session.setAccess,
                    ),
                  ),
                ),
                child: const Text('Open transfer'),
              ),
            ),
          ),
        ),
      ),
    );
    await openTransfer(tester);
  }

  /// Scan the original device's QR code. The camera is not part of what this
  /// holds, so the scanner route is popped with the code instead of opening.
  Future<void> scan(WidgetTester tester) async {
    await tester.tap(find.byKey(const Key('scan-account-transfer')));
    tester.state<NavigatorState>(find.byType(Navigator)).pop('{"version":1}');
    await tester.pump();
    await tester.pump(const Duration(seconds: 2));
    await tester.pumpAndSettle();
  }

  testWidgets('a new device adopts the account before moving its notes', (
    tester,
  ) async {
    await library.createNote();
    library.calls.clear();
    await pumpScreen(tester);

    await scan(tester);

    // Nobody was asked to create an account: the transferred one came first.
    expect(prompts, isEmpty);
    expect(library.hasAccount, isTrue);
    expect(
      library.calls,
      containsAllInOrder([
        'adoptAccountTransfer()',
        'flush()',
        'connect(https://relay.example)',
      ]),
    );
    expect(session.profile.url, server);
    expect(find.textContaining('Account added'), findsOneWidget);
    // Still the guest's screen: this device has an account now, but this
    // screen is the one the transfer finished on.
    expect(find.text('Add another device'), findsNothing);
    expect(find.byKey(const Key('begin-account-transfer')), findsNothing);
    expect(find.byKey(const Key('scan-account-transfer')), findsNothing);
  });

  testWidgets('a move that fails after adopting says what state it is in', (
    tester,
  ) async {
    await library.createNote();
    library.calls.clear();
    profiles.failMove = true;
    await pumpScreen(tester);

    await scan(tester);

    expect(prompts, isEmpty);
    expect(library.hasAccount, isTrue);
    expect(session.profile.url, isNull);
    expect(find.textContaining('This device has your account'), findsOneWidget);
    expect(
      find.textContaining('the transfer to https://relay.example did not'),
      findsOneWidget,
    );
    // The notes stayed here, so the guest's side is the one that can finish:
    // the scan is offered again and Cancel still leaves the pairing.
    expect(find.byKey(const Key('scan-account-transfer')), findsOneWidget);
    expect(find.text('Cancel'), findsOneWidget);
    expect(find.text('Add another device'), findsNothing);
    expect(find.byKey(const Key('begin-account-transfer')), findsNothing);
    expect(find.textContaining('Waiting for approval'), findsNothing);

    // Pairing again retries the move. The account is already here, so it is
    // not adopted twice, and the collection is moved, not copied.
    profiles.failMove = false;
    await scan(tester);
    expect(prompts, isEmpty);
    expect(session.profile.url, server);
    expect(library.notes, hasLength(1));
    expect(
      library.calls.where((c) => c == 'adoptAccountTransfer()'),
      hasLength(1),
    );
    expect(find.textContaining('Account added'), findsOneWidget);
  });

  testWidgets('a connection failure after the move keeps the profile on the '
      'server', (tester) async {
    await library.createNote();
    library.calls.clear();
    library.refuseConnection = true;
    await pumpScreen(tester);

    await scan(tester);

    expect(prompts, isEmpty);
    expect(library.hasAccount, isTrue);
    // The move itself finished: the profile is on the relay, the notes with
    // it, and only the connection failed.
    expect(session.profile.url, server);
    expect(library.notes, hasLength(1));
    expect(find.textContaining('This device has your account'), findsOneWidget);
    expect(
      find.textContaining('the transfer to https://relay.example did not'),
      findsOneWidget,
    );
    expect(find.text('Cancel'), findsOneWidget);

    // Retrying pairs again, adopts nothing and connects the same profile.
    library.refuseConnection = false;
    await scan(tester);
    expect(prompts, isEmpty);
    expect(session.profile.url, server);
    expect(library.notes, hasLength(1));
    expect(
      library.calls.where((c) => c == 'adoptAccountTransfer()'),
      hasLength(1),
    );
    expect(find.textContaining('connected to'), findsOneWidget);
  });

  testWidgets('a pairing resumed after Cancel finishes without adopting '
      'again', (tester) async {
    await library.createNote();
    library.calls.clear();
    profiles.failMove = true;
    await pumpScreen(tester);

    await scan(tester);
    expect(library.hasAccount, isTrue);
    expect(session.profile.url, isNull);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(find.byType(AccountTransferScreen), findsNothing);

    // The screen opened again — after Cancel or after a restart — and pairs
    // with the same account. It is already on this device, so adopting it
    // again would be refused; only the move is left.
    profiles.failMove = false;
    await openTransfer(tester);
    await scan(tester);

    expect(prompts, isEmpty);
    expect(session.profile.url, server);
    expect(library.notes, hasLength(1));
    expect(
      library.calls.where((c) => c == 'adoptAccountTransfer()'),
      hasLength(1),
    );
    expect(find.textContaining('Account added'), findsOneWidget);
    expect(find.textContaining('already has an account'), findsNothing);
  });
}
