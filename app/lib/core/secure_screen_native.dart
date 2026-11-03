import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

const _channel = MethodChannel('eu.fraisl.easynotes/secure_screen');

/// Sets or clears `FLAG_SECURE` through the Android host.
///
/// Only Android has the flag. iOS has no equivalent screenshot block, so this
/// is a no-op there and the words remain capturable on that platform.
Future<void> setSecureScreen(bool secure) async {
  if (defaultTargetPlatform != TargetPlatform.android) return;
  try {
    await _channel.invokeMethod<void>('setSecure', {'secure': secure});
  } on MissingPluginException {
    // Widget tests and hosts with no Android side: there is no window to flag.
  } on PlatformException {
    // The window is not there to take the flag; the screen still works.
  }
}
