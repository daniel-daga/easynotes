/// Screenshot blocking for the screens that show a secret.
///
/// The claim is narrow by design: on Android the window is flagged, and on
/// every other platform the call is a no-op rather than a pretend cover.
library;

import 'support/fake_note_library.dart';
import 'package:easynotes/core/secure_screen.dart';
import 'package:easynotes/ui/account_screen.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const channel = MethodChannel('eu.fraisl.easynotes/secure_screen');
  final calls = <Object?>[];
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  setUp(() {
    calls.clear();
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call.arguments);
      return null;
    });
  });

  tearDown(() {
    debugDefaultTargetPlatformOverride = null;
    messenger.setMockMethodCallHandler(channel, null);
  });

  test('on Android the flag is set and cleared through the channel', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;

    await setSecureScreen(true);
    await setSecureScreen(false);

    expect(calls, [
      {'secure': true},
      {'secure': false},
    ]);
  });

  test('elsewhere the channel is never asked', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.iOS;

    await setSecureScreen(true);
    await setSecureScreen(false);

    expect(calls, isEmpty);
  });

  testWidgets('the recovery phrase screen flags the window while it is shown', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: RecoveryPhraseScreen(
          words: FakeNoteLibrary.phrase,
          ask: const [1, 2, 3],
        ),
      ),
    );
    expect(calls, [
      {'secure': true},
    ]);

    await tester.pumpWidget(const SizedBox());
    expect(calls.last, {'secure': false});
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  testWidgets('the restore form flags the window while it is shown', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(home: AdoptAccountScreen(library: FakeNoteLibrary())),
    );
    expect(calls, [
      {'secure': true},
    ]);

    await tester.pumpWidget(const SizedBox());
    expect(calls.last, {'secure': false});
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));
}
