/// A launch that cannot finish still has to say why.
///
/// Settings written by a newer build, a corrupt stored value or a keychain key
/// that is not a key all throw before the first `runApp`, and a launch that
/// throws there draws nothing at all: the launch screen stays until the app is
/// force-stopped. This drives the real `main()` through the real
/// [RelayProfiles.load] and asks what is on screen afterwards.
library;

import 'dart:convert';

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
  }, variant: TargetPlatformVariant.only(TargetPlatform.linux));
}
