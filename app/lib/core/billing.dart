/// Minting access tokens from billing, for a hosted relay.
///
/// A hosted relay's access token belongs to one device, so the owner's token
/// cannot be handed to a guest or a new device. Billing mints one for each:
/// the app asks, proving its own token, and puts the answer in the invite link
/// or the transfer QR. A build with no [billingBase] never calls billing and
/// keeps using the profile's own key, which is how a self-hosted relay works.
library;

import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

import 'relay_profiles.dart';

/// Where this build's billing service is, or empty when it has none.
const billingBase = String.fromEnvironment('EASYNOTES_BILLING');

/// A billing call that failed, with a sentence the UI can show.
class BillingException implements Exception {
  const BillingException(this.message);

  final String message;

  @override
  String toString() => message;
}

class BillingClient {
  BillingClient({String base = billingBase, http.Client? client})
    : _base = base.replaceFirst(RegExp(r'/+$'), ''),
      _client = client ?? http.Client();

  /// The client the screens use: disabled unless the build named a service.
  factory BillingClient.fromEnvironment() => BillingClient();

  final String _base;
  final http.Client _client;

  /// Whether this build has a billing service to ask.
  bool get enabled => _base.isNotEmpty;

  /// A token for someone invited to a note, minted from this device's
  /// [deviceToken].
  Future<String> guestToken(String deviceToken) =>
      _mint('guest-token', deviceToken);

  /// A token for a device being added to this account.
  Future<String> deviceToken(String deviceToken) =>
      _mint('device-token', deviceToken);

  /// The token, or a [BillingException]. The device token is the only thing
  /// sent: no note, no account, no phrase.
  Future<String> _mint(String path, String deviceToken) async {
    if (!enabled) {
      // Only a caller that ignored [enabled] gets here; no request is made.
      throw StateError('This build has no billing service.');
    }
    final http.Response response;
    try {
      response = await _client
          .post(
            Uri.parse('$_base/v1/$path'),
            headers: {'authorization': 'Bearer $deviceToken'},
          )
          .timeout(const Duration(seconds: 10));
    } on TimeoutException {
      throw const BillingException(
        'The billing service did not answer. Try again in a moment.',
      );
    } on Object {
      throw const BillingException(
        'The billing service could not be reached. Check your connection and '
        'try again.',
      );
    }
    if (response.statusCode < 200 || response.statusCode > 299) {
      throw const BillingException(
        'The billing service could not make a token. Try again later.',
      );
    }
    Object? body;
    try {
      body = jsonDecode(response.body);
    } on FormatException {
      body = null;
    }
    final access = body is Map ? body['access'] : null;
    final token = access is String ? normalizeAccessKey(access) : null;
    if (token == null) {
      throw const BillingException(
        'The billing service sent an answer this app does not understand. '
        'Try again later.',
      );
    }
    return token;
  }
}
