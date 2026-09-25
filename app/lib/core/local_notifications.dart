/// The system's notifications, set up once for everything that posts them:
/// reminders ([ReminderScheduler]) and people joining a note
/// ([SystemJoinAlerts]).
library;

import 'package:flutter/foundation.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import 'note_library.dart';

Future<void>? _initialized;

/// Initialise the plugin, once however many callers ask. It never asks for
/// permission: each kind of notification asks at the gesture that needs it.
Future<void> initializeLocalNotifications() =>
    _initialized ??= FlutterLocalNotificationsPlugin().initialize(
      settings: const InitializationSettings(
        android: AndroidInitializationSettings('@mipmap/ic_launcher'),
        iOS: DarwinInitializationSettings(
          requestAlertPermission: false,
          requestBadgePermission: false,
          requestSoundPermission: false,
        ),
        macOS: DarwinInitializationSettings(
          requestAlertPermission: false,
          requestBadgePermission: false,
          requestSoundPermission: false,
        ),
        linux: LinuxInitializationSettings(defaultActionName: 'Open'),
        windows: WindowsInitializationSettings(
          appName: 'EasyNotes',
          appUserModelId: 'EasyNotes.EasyNotes.Desktop',
          guid: '45b9c55e-998e-4aca-9e37-a09f52545141',
        ),
      ),
    );

/// A notification on this device when somebody joins one of its notes.
///
/// It is posted by whichever poll lets them in (`InviteBanners`), so it
/// arrives while the app is running or has only just been put away. Nothing
/// wakes a closed app: that needs push, which the relay does not have.
class SystemJoinAlerts implements JoinAlerts {
  SystemJoinAlerts(this._library);

  final NoteLibrary _library;
  final _notifications = FlutterLocalNotificationsPlugin();

  @override
  Future<void> requestPermission() async {
    await initializeLocalNotifications();
    if (kIsWeb) {
      await _notifications
          .resolvePlatformSpecificImplementation<
            WebFlutterLocalNotificationsPlugin
          >()
          ?.requestNotificationsPermission();
      return;
    }
    await _notifications
        .resolvePlatformSpecificImplementation<
          AndroidFlutterLocalNotificationsPlugin
        >()
        ?.requestNotificationsPermission();
    await _notifications
        .resolvePlatformSpecificImplementation<
          IOSFlutterLocalNotificationsPlugin
        >()
        ?.requestPermissions(alert: true, sound: true);
  }

  @override
  Future<void> joined({
    required String inviteId,
    required String noteId,
    required String name,
  }) async {
    await initializeLocalNotifications();
    final title = _library.notes
        .where((n) => n.id == noteId)
        .map((n) => n.title)
        .firstOrNull;
    final text = joinedNotificationText(name, title ?? '');
    await _notifications.show(
      id: _id(inviteId),
      title: text.title,
      body: text.body,
      notificationDetails: kIsWeb
          ? null
          : const NotificationDetails(
              android: AndroidNotificationDetails(
                'people',
                'People joining',
                channelDescription:
                    'When someone opens a link you sent and joins your note',
              ),
              iOS: DarwinNotificationDetails(),
              macOS: DarwinNotificationDetails(),
              linux: LinuxNotificationDetails(),
            ),
      payload: '$_payloadPrefix$noteId',
    );
  }

  /// One per invite, so two people joining are two notifications. Kept out of
  /// the range reminders hash into by being negative.
  static int _id(String inviteId) {
    var hash = 0x811c9dc5;
    for (final unit in inviteId.codeUnits) {
      hash = (hash ^ unit) * 0x01000193 & 0x7fffffff;
    }
    return -(hash == 0 ? 1 : hash);
  }
}

const _payloadPrefix = 'easynotes-joined:';
