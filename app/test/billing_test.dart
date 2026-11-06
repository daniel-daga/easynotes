import 'dart:convert';

import 'package:easynotes/core/billing.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// A base64url value of [bytes] bytes, as the core spells keys and tokens.
String token(int bytes, [int fill = 7]) =>
    base64Url.encode(List<int>.filled(bytes, fill)).replaceAll('=', '');

void main() {
  final owner = token(32);
  final guest = '${token(16, 3)}.${token(32, 4)}';
  final device = '${token(16, 5)}.${token(32, 6)}';

  test('a configured service mints a guest token', () async {
    late http.Request seen;
    final billing = BillingClient(
      base: 'https://billing.example/',
      client: MockClient((request) async {
        seen = request;
        return http.Response('{"access":"$guest"}', 200);
      }),
    );

    expect(billing.enabled, isTrue);
    expect(await billing.guestToken(owner), guest);
    expect(seen.method, 'POST');
    expect(seen.url.toString(), 'https://billing.example/v1/guest-token');
    expect(seen.headers['authorization'], 'Bearer $owner');
  });

  test('a device token comes from its own path', () async {
    late http.Request seen;
    final billing = BillingClient(
      base: 'https://billing.example',
      client: MockClient((request) async {
        seen = request;
        return http.Response('{"access":"$device"}', 200);
      }),
    );

    expect(await billing.deviceToken(owner), device);
    expect(seen.url.path, '/v1/device-token');
    expect(seen.headers['authorization'], 'Bearer $owner');
  });

  test('a refusal is a sentence, and no token', () async {
    final billing = BillingClient(
      base: 'https://billing.example',
      client: MockClient((_) async => http.Response('nope', 403)),
    );

    await expectLater(
      billing.guestToken(owner),
      throwsA(
        isA<BillingException>().having(
          (e) => e.message,
          'message',
          contains('billing service'),
        ),
      ),
    );
  });

  test('an answer without a token is a sentence too', () async {
    for (final body in [
      'not json',
      '{}',
      '{"access":"short"}',
      // A bare operator key is not a token this app may put in a link.
      '{"access":"${token(32)}"}',
    ]) {
      final billing = BillingClient(
        base: 'https://billing.example',
        client: MockClient((_) async => http.Response(body, 200)),
      );
      await expectLater(
        billing.deviceToken(owner),
        throwsA(isA<BillingException>()),
        reason: body,
      );
    }
  });

  test('no billing base is disabled, and asks nobody', () async {
    var asked = 0;
    final billing = BillingClient(
      base: '',
      client: MockClient((_) async {
        asked++;
        return http.Response('{"access":"$guest"}', 200);
      }),
    );

    expect(billing.enabled, isFalse);
    await expectLater(billing.guestToken(owner), throwsStateError);
    expect(asked, 0);
  });
}
