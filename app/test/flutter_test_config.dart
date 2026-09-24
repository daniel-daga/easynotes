import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

/// Every test runs on a platform with no share sheet, as a desktop is, unless
/// it mocks one of its own. An unanswered platform channel never replies
/// inside a widget test's fake clock, so without this a tap on "Send link"
/// would wait forever for a sheet that is not there.
Future<void> testExecutable(FutureOr<void> Function() testMain) async {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUp(noShareSheet);
  await testMain();
}

void noShareSheet() => TestDefaultBinaryMessengerBinding
    .instance
    .defaultBinaryMessenger
    .setMockMethodCallHandler(
      const MethodChannel('eu.fraisl.easynotes/content_sharing'),
      (call) async => false,
    );
