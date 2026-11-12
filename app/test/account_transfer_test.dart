/// Adding a device from an approved QR pairing, against fakes.
///
/// This holds the order the screen used to get wrong: on a device with no
/// account, moving the collection first asked the person to create an account,
/// and the adopted account was then refused. Adopting first is what lets the
/// move and the connection happen at all.
library;

import 'dart:async';
import 'dart:convert';

import 'package:easynotes/core/billing.dart';
import 'support/fake_note_library.dart';
import 'package:easynotes/core/bridge_error.dart';
import 'package:easynotes/core/invites.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/relay_session.dart';
import 'package:easynotes/ui/account_transfer_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

const server = 'https://relay.example';

/// A base64url value of [bytes] bytes, as the core spells keys and tokens.
String token(int bytes, [int fill = 7]) =>
    base64Url.encode(List<int>.filled(bytes, fill)).replaceAll('=', '');

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

  /// Makes the next polls fail as a relay that lost the pairing does.
  bool transferGone = false;

  @override
  Future<({String relayUrl, String? relayAccess})?>
  pollAccountTransferResult() async {
    calls.add('pollAccountTransferResult()');
    if (transferGone) {
      throw bridgeError('transfer_gone', transferGoneMessage);
    }
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
  ) async {
    begunAccess = relayAccess;
    return '{"version":1}';
  }

  /// The access token the QR was made with.
  String? begunAccess;

  /// The code a paired device shows once its owner's request arrives.
  String? requestCode;

  /// Holds the first request-code poll open, as a slow relay does, so a poll
  /// can still be in flight when the owner approves.
  Completer<void>? holdFirstPoll;

  /// Makes a poll that resumes later come back with an error.
  bool failRequestCode = false;

  int _pollCount = 0;

  @override
  Future<String?> accountTransferRequestCode() async {
    calls.add('accountTransferRequestCode()');
    if (holdFirstPoll != null && _pollCount++ == 0) {
      await holdFirstPoll!.future;
    }
    if (failRequestCode) throw 'the relay is unreachable now';
    return requestCode;
  }

  @override
  Future<void> approveAccountTransfer() async {
    calls.add('approveAccountTransfer()');
  }

  @override
  Future<void> cancelAccountTransfer() async {
    calls.add('cancelAccountTransfer()');
  }
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
  Future<void> pumpScreen(
    WidgetTester tester, {
    RelayProfile? profile,
    BillingClient? billing,
  }) async {
    final shown = profile ?? session.profile;
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
                      profile: shown,
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
                      billing: billing,
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

  testWidgets('the transfer screen flags the window while it is open', (
    tester,
  ) async {
    const channel = MethodChannel('eu.fraisl.easynotes/secure_screen');
    final calls = <Object?>[];
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call.arguments);
      return null;
    });
    addTearDown(() {
      messenger.setMockMethodCallHandler(channel, null);
    });

    await pumpScreen(tester);
    expect(calls, [
      {'secure': true},
    ]);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(calls.last, {'secure': false});
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

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
    // The transfer finished, so the screen going away did not cancel it.
    expect(library.calls.where((c) => c == 'cancelAccountTransfer()'), isEmpty);
  });

  testWidgets('a guest stops polling when the relay lost the pairing', (
    tester,
  ) async {
    library.transferGone = true;
    await pumpScreen(tester);

    await scan(tester);

    // The honest reason, not "the invite was withdrawn", and the waiting
    // state is gone because no amount of polling will finish this transfer.
    expect(find.text(transferGoneMessage), findsOneWidget);
    expect(find.textContaining('Waiting for approval'), findsNothing);
    expect(find.textContaining('withdrawn'), findsNothing);
    // Only a new code can start again, so the scan is offered.
    expect(find.byKey(const Key('scan-account-transfer')), findsOneWidget);

    // The timer itself is gone, not merely quiet for one tick.
    final polls = library.calls
        .where((c) => c == 'pollAccountTransferResult()')
        .length;
    await tester.pump(const Duration(seconds: 6));
    expect(
      library.calls.where((c) => c == 'pollAccountTransferResult()'),
      hasLength(polls),
    );
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
    // Cancel closed the pairing, and the dispose that followed did not close
    // it again.
    expect(
      library.calls.where((c) => c == 'cancelAccountTransfer()'),
      hasLength(1),
    );

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

  testWidgets('the owner approves a request it started, then the QR closes', (
    tester,
  ) async {
    library.setUpSharing();
    library.requestCode = '246810';
    // One poll is still waiting on the relay when the code arrives from a
    // later one, and comes back with an error after the approval.
    library.holdFirstPoll = Completer<void>();
    await pumpScreen(
      tester,
      profile: const RelayProfile(id: 'owner', url: server),
    );

    // The QR is the account, so the only request to approve is one started
    // here, a moment ago, on a device in front of the owner.
    expect(find.textContaining('The QR code is the account'), findsOneWidget);
    expect(
      find.textContaining(
        'never one from a code someone read to you or sent you',
      ),
      findsOneWidget,
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 2));
    await tester.pump();

    // The first poll is still out, so the QR is up but no code yet. The
    // second poll answers while the first is in flight.
    expect(find.byKey(const Key('account-transfer-qr')), findsOneWidget);
    expect(find.text('246810'), findsNothing);
    await tester.pump(const Duration(seconds: 2));
    await tester.pump();

    expect(find.byKey(const Key('account-transfer-qr')), findsOneWidget);
    expect(find.text('246810'), findsOneWidget);
    expect(find.byKey(const Key('approve-account-transfer')), findsOneWidget);

    await tester.ensureVisible(
      find.byKey(const Key('approve-account-transfer')),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('approve-account-transfer')));
    await tester.pump();
    await tester.pump();

    expect(find.byKey(const Key('account-transfer-qr')), findsNothing);
    expect(find.text('246810'), findsNothing);
    expect(find.byKey(const Key('approve-account-transfer')), findsNothing);
    expect(library.calls, contains('approveAccountTransfer()'));
    // Nothing of the pairing is rendered after approval — not the QR, not the
    // code, not the intro that asked for them: the completion message is what
    // is left. (The secret itself is dropped in Rust; this holds the screen.)
    expect(find.byKey(const Key('begin-account-transfer')), findsNothing);
    expect(find.textContaining('The QR code is the account'), findsNothing);
    expect(find.textContaining('Transfer sent securely'), findsOneWidget);

    // The slow poll comes back with an error after the approval: it must not
    // replace the approval message.
    library.failRequestCode = true;
    library.holdFirstPoll!.complete();
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('Transfer sent securely'), findsOneWidget);
    expect(find.textContaining('relay is unreachable'), findsNothing);

    // Approval leaves nothing to cancel when the screen goes away.
    await tester.pumpWidget(const SizedBox());
    expect(library.calls.where((c) => c == 'cancelAccountTransfer()'), isEmpty);
  });

  testWidgets('leaving the screen before approval cancels the pairing', (
    tester,
  ) async {
    await pumpScreen(tester);
    library.calls.clear();

    // The screen was left without Cancel — by a system back, say: the
    // pairing must not stay live for its full five minutes.
    await tester.pumpWidget(const SizedBox());
    expect(
      library.calls.where((c) => c == 'cancelAccountTransfer()'),
      hasLength(1),
    );
  });

  testWidgets('the QR carries a fresh token for the new device', (
    tester,
  ) async {
    final owner = '${token(16, 7)}.${token(32, 8)}';
    final device = '${token(16, 3)}.${token(32, 4)}';
    final requests = <http.Request>[];
    library.setUpSharing();
    await pumpScreen(
      tester,
      profile: RelayProfile(id: 'owner', url: server, access: owner),
      billing: BillingClient(
        base: 'https://billing.example',
        client: MockClient((request) async {
          requests.add(request);
          return http.Response('{"access":"$device"}', 200);
        }),
      ),
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump();

    // The QR is the pairing, so it must not carry this device's own token.
    expect(library.begunAccess, device);
    expect(find.byKey(const Key('account-transfer-qr')), findsOneWidget);
    expect(requests.single.url.path, '/v1/device-token');
    expect(requests.single.headers['authorization'], 'Bearer $owner');

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a bare operator key is never sent to billing', (tester) async {
    // A self-hosted relay's key in a build that has billing configured: the
    // QR keeps it, and billing is not asked to mint from it.
    final operator = token(32);
    var asked = 0;
    library.setUpSharing();
    await pumpScreen(
      tester,
      profile: RelayProfile(id: 'owner', url: server, access: operator),
      billing: BillingClient(
        base: 'https://billing.example',
        client: MockClient((_) async {
          asked++;
          return http.Response('{"access":"${token(32)}"}', 200);
        }),
      ),
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump();

    expect(library.begunAccess, operator);
    expect(find.byKey(const Key('account-transfer-qr')), findsOneWidget);
    expect(asked, 0);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a billing failure shows its sentence and no QR', (tester) async {
    library.setUpSharing();
    await pumpScreen(
      tester,
      profile: RelayProfile(
        id: 'owner',
        url: server,
        access: '${token(16, 7)}.${token(32, 8)}',
      ),
      billing: BillingClient(
        base: 'https://billing.example',
        client: MockClient((_) async => http.Response('nope', 503)),
      ),
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump();

    expect(find.byKey(const Key('account-transfer-qr')), findsNothing);
    expect(find.textContaining('billing service'), findsOneWidget);
    expect(library.begunAccess, isNull);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a device token with no billing shows no QR', (tester) async {
    library.setUpSharing();
    await pumpScreen(
      tester,
      profile: RelayProfile(
        id: 'owner',
        url: server,
        access: '${token(16, 7)}.${token(32, 8)}',
      ),
      // No billing address in this build.
      billing: BillingClient(base: ''),
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump();

    expect(find.byKey(const Key('account-transfer-qr')), findsNothing);
    expect(find.textContaining("own key can't be put in a QR"), findsOneWidget);
    expect(library.begunAccess, isNull);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a bare operator key with billing off still makes the QR', (
    tester,
  ) async {
    final operator = token(32);
    library.setUpSharing();
    await pumpScreen(
      tester,
      profile: RelayProfile(id: 'owner', url: server, access: operator),
      billing: BillingClient(base: ''),
    );

    await tester.tap(find.byKey(const Key('begin-account-transfer')));
    await tester.pump();
    await tester.pump();

    expect(library.begunAccess, operator);
    expect(find.byKey(const Key('account-transfer-qr')), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
  });
}
