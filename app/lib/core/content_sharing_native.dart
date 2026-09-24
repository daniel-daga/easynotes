import 'package:flutter/services.dart';

const _channel = MethodChannel('eu.fraisl.easynotes/content_sharing');

Future<bool> shareContent(String text) async {
  try {
    return await _channel.invokeMethod<bool>('share', {'text': text}) ?? false;
  } on MissingPluginException {
    return false;
  } on PlatformException {
    return false;
  }
}
