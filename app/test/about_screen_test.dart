/// Settings → About and privacy: what the notice says depends on the profile.
library;

import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/ui/about_screen.dart';
import 'package:flutter/foundation.dart' show LicenseRegistry;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  Future<void> pump(WidgetTester tester, RelayProfile profile) =>
      tester.pumpWidget(MaterialApp(home: AboutScreen(profile: profile)));

  testWidgets('a local profile says nothing leaves the device, and that '
      'nothing backs it up', (tester) async {
    await pump(tester, const RelayProfile(id: 'local', url: null));
    expect(find.textContaining('connects to no server'), findsOneWidget);
    expect(find.textContaining('not backed up anywhere'), findsOneWidget);
    expect(find.textContaining('IP address'), findsNothing);
  });

  testWidgets('a relay profile names the relay and what it can see', (
    tester,
  ) async {
    await pump(
      tester,
      const RelayProfile(id: 'r', url: 'https://relay.example'),
    );
    expect(find.textContaining('https://relay.example'), findsOneWidget);
    expect(find.textContaining('IP address'), findsOneWidget);
    expect(find.textContaining('cannot read'), findsOneWidget);
  });

  testWidgets('a build given no policy or contact says so rather than '
      'showing blanks', (tester) async {
    await pump(tester, const RelayProfile(id: 'local', url: null));
    expect(find.text('Development build'), findsOneWidget);
    await tester.scrollUntilVisible(
      find.textContaining('not published with a privacy policy'),
      100,
    );
    expect(
      find.textContaining('not published with a privacy policy'),
      findsOneWidget,
    );
  });

  testWidgets('the licence and where the source is are stated', (tester) async {
    await pump(tester, const RelayProfile(id: 'local', url: null));
    await tester.scrollUntilVisible(find.byKey(const Key('source-url')), 100);
    expect(
      find.textContaining('Affero General Public License'),
      findsOneWidget,
    );
    expect(
      find.byWidgetPredicate((w) => w is SelectableText && w.data == sourceUrl),
      findsOneWidget,
    );
  });

  testWidgets('the licence page is given the bundled font', (tester) async {
    registerBundledLicenses();
    final licenses = await tester.runAsync(
      () => LicenseRegistry.licenses.toList(),
    );
    final roboto = licenses!.where((l) => l.packages.contains('Roboto'));
    expect(roboto, hasLength(1));
    expect(
      roboto.single.paragraphs.map((p) => p.text).join(' '),
      contains('Apache License'),
    );
  });
}
