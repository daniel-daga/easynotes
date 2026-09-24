import 'package:flutter/services.dart';

import 'invite_links.dart';

/// Delivers Android invite intents to Flutter on cold and warm launches.
class DeepLinks {
  static const _methods = MethodChannel('eu.fraisl.easynotes/deep_links');
  static const _events = EventChannel('eu.fraisl.easynotes/deep_links/events');

  static Stream<String> get invites => _events
      .receiveBroadcastStream()
      .where((value) => value is String)
      .cast<String>()
      .map(inviteLinkFrom)
      .where((value) => value != null)
      .cast<String>();

  static Future<String?> initialInvite() async {
    try {
      final value = await _methods.invokeMethod<String>('getInitialLink');
      return value == null ? null : inviteLinkFrom(value);
    } on MissingPluginException {
      return null;
    }
  }
}
