import 'package:easynotes/core/invites.dart';
import 'package:easynotes/core/local_notifications.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  setUpAll(AndroidFlutterLocalNotificationsPlugin.registerWith);

  test('the notification text is generic', () {
    final text = joinedNotificationText();
    expect(text.title, 'EasyNotes');
    expect(text.body, 'Someone joined a shared note.');
  });

  test('join notifications are private and update existing channels', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    const channel = MethodChannel('dexterous.com/flutter/local_notifications');
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    final shown = <Map<dynamic, dynamic>>[];
    messenger.setMockMethodCallHandler(channel, (call) async {
      if (call.method == 'initialize') return true;
      if (call.method == 'show') shown.add(call.arguments as Map);
      return null;
    });
    addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
    final alerts = SystemJoinAlerts();
    addTearDown(alerts.dispose);

    await alerts.joined(
      inviteId: 'invite-1',
      noteId: 'note-1',
      name: 'Private member name',
    );

    final notification = shown.single;
    expect(notification['title'], 'EasyNotes');
    expect(notification['body'], 'Someone joined a shared note.');
    expect(notification.toString(), isNot(contains('Private member name')));
    expect(notification['payload'], 'easynotes-joined:note-1');
    expect(notification['id'], isNegative);
    final android = notification['platformSpecifics'] as Map;
    expect(android['channelId'], 'people');
    expect(android['visibility'], NotificationVisibility.private.index);
    expect(
      android['channelAction'],
      AndroidNotificationChannelAction.update.index,
    );
  });
}
