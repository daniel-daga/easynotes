/// A launch that cannot finish still has to say why.
///
/// Settings written by a newer build, a corrupt stored value or a keychain key
/// that is not a key all throw before the first `runApp`, and a launch that
/// throws there draws nothing at all: the launch screen stays until the app is
/// force-stopped. This drives the real startup path and asks what is on screen
/// afterwards.
library;

import 'dart:convert';

import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/main.dart' as app;
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  // Linux is a platform with no deep links and no share sheet, so nothing
  // before the settings read reaches a plugin.
  testWidgets('settings from a newer build are a screen, not a blank launch', (
    tester,
  ) async {
    FlutterSecureStorage.setMockInitialValues({
      'easynotes.relay-profiles': jsonEncode({
        'version': RelayProfiles.version + 1,
        'selected': 'relay',
        'profiles': [
          {'id': 'relay', 'url': 'https://relay.example'},
        ],
      }),
    });

    await app.main();
    await tester.pump();

    expect(find.text('EasyNotes could not start'), findsOneWidget);
    final shown = tester.widget<SelectableText>(
      find.byKey(const Key('startup-error')),
    );
    expect(shown.data, contains('newer version of EasyNotes'));
    // One way out that works on a phone and one that names the browser's own
    // action, in the order a person should try them.
    final advice = tester
        .widget<Text>(find.byKey(const Key('startup-advice')))
        .data!;
    expect(advice, contains('open it again'));
    expect(advice, contains('update'));
    expect(advice, contains('support'));
    expect(advice, contains('reinstalling'));
    expect(advice, contains('clearing'));
    expect(advice, contains('browser'));
  }, variant: TargetPlatformVariant.only(TargetPlatform.linux));

  // The real `main` reaches this failure through the keychain and the native
  // bridge, which a widget test's fake clock cannot drive; the startup step
  // itself is the same `MasterKeyStore` call.
  testWidgets(
    'a truncated key in the keychain is a screen that does not quote it',
    (tester) async {
      final key = MasterKeyStore.newKey();
      final stored = base64Encode(key).substring(0, 10);
      FlutterSecureStorage.setMockInitialValues({
        'easynotes.master-key': stored,
      });

      await app.runStartup(() async {
        await MasterKeyStore().forDirectory('/none');
      });
      await tester.pump();

      expect(find.text('EasyNotes could not start'), findsOneWidget);
      final shown = tester
          .widget<SelectableText>(find.byKey(const Key('startup-error')))
          .data!;
      expect(shown, contains('not valid base64'));
      expect(shown, isNot(contains(stored)));
    },
  );
}
