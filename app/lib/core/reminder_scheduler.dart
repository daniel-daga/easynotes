/// The platform edge of reminders.
///
/// Rust owns encrypted reminder data and merge semantics. This file reads that
/// already-synced data and mirrors it into the operating system's local
/// scheduler; no reminder content or time reaches the relay as a consequence.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_timezone/flutter_timezone.dart';
import 'package:timezone/data/latest_all.dart' as tz_data;
import 'package:timezone/timezone.dart' as tz;

import 'note_library.dart';

class ReminderScheduler {
  ReminderScheduler(this._library);

  static ReminderScheduler? instance;

  final NoteLibrary _library;
  final FlutterLocalNotificationsPlugin _notifications =
      FlutterLocalNotificationsPlugin();
  StreamSubscription<List<Reminder>>? _updates;
  final Map<String, Timer> _inProcessTimers = {};
  bool _started = false;

  /// Start after the library is open. This never asks for notification
  /// permission: that prompt belongs to the explicit "Add reminder" gesture.
  Future<void> start() async {
    if (_started) return;
    _started = true;
    instance = this;
    tz_data.initializeTimeZones();
    final zone = await deviceTimezone();
    try {
      tz.setLocalLocation(tz.getLocation(zone));
    } on ArgumentError {
      tz.setLocalLocation(tz.UTC);
    }
    await _notifications.initialize(
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
    _updates = _library.watchReminders().listen((_) => unawaited(reconcile()));
    await reconcile();
  }

  /// Ask only after a person explicitly creates a reminder. A failed request is
  /// not a failed reminder: the account document still syncs and this device
  /// retries scheduling when permission changes or it launches again.
  Future<bool> requestPermission() async {
    if (kIsWeb) {
      final web = _notifications
          .resolvePlatformSpecificImplementation<
            WebFlutterLocalNotificationsPlugin
          >();
      return await web?.requestNotificationsPermission() ?? false;
    }
    final android = _notifications
        .resolvePlatformSpecificImplementation<
          AndroidFlutterLocalNotificationsPlugin
        >();
    final ios = _notifications
        .resolvePlatformSpecificImplementation<
          IOSFlutterLocalNotificationsPlugin
        >();
    final androidGranted = await android?.requestNotificationsPermission();
    if (androidGranted ?? false) {
      await android?.requestExactAlarmsPermission();
    }
    final iosGranted = await ios?.requestPermissions(
      alert: true,
      badge: false,
      sound: true,
    );
    // Null means another platform, where initialization is sufficient.
    return androidGranted ?? iosGranted ?? true;
  }

  /// Mirror encrypted reminders into native pending notifications. Trashed
  /// notes are deliberately excluded; restoring the note makes its reminder
  /// eligible again without rewriting its account-document entry.
  Future<void> reconcile() async {
    if (!_started) return;
    final noteById = {for (final note in _library.notes) note.id: note};
    final reminders = [
      for (final reminder in _library.reminders)
        if (noteById[reminder.noteId]?.trashed != true) reminder,
    ];

    // Rank by the next actual firing, not the original timestamp. Expired
    // one-shot reminders have no future occurrence and must not occupy one of
    // the operating system's limited pending-notification slots.
    final scheduled = <({Reminder reminder, tz.TZDateTime occurrence})>[];
    for (final reminder in reminders) {
      final occurrence = _nextOccurrence(
        reminder,
        _location(reminder.timezone),
      );
      if (occurrence != null) {
        scheduled.add((reminder: reminder, occurrence: occurrence));
      }
    }
    scheduled.sort((a, b) => a.occurrence.compareTo(b.occurrence));

    // iOS retains only 64 pending local notifications. Its native repeating
    // triggers count as one, so this keeps the nearest 64 reminder identities.
    final wanted = scheduled.take(64).map((entry) => entry.reminder).toList();
    if (kIsWeb || defaultTargetPlatform == TargetPlatform.linux) {
      await _reconcileInProcess(wanted, noteById);
      return;
    }
    final pending = await _notifications.pendingNotificationRequests();
    final existing = <String, int>{
      for (final request in pending)
        if (request.payload?.startsWith(_payloadPrefix) ?? false)
          request.payload!.substring(_payloadPrefix.length): request.id,
    };
    final wantedIds = {for (final reminder in wanted) reminder.id};
    for (final request in pending) {
      final payload = request.payload;
      if ((payload?.startsWith(_payloadPrefix) ?? false) &&
          !wantedIds.contains(payload!.substring(_payloadPrefix.length))) {
        await _notifications.cancel(id: request.id);
      }
    }

    final used = existing.values.toSet();
    for (final reminder in wanted) {
      final id = existing[reminder.id] ?? _notificationId(reminder.id, used);
      used.add(id);
      final note = noteById[reminder.noteId];
      final title = note?.title.trim();
      try {
        await _schedule(
          id: id,
          reminder: reminder,
          title: title == null || title.isEmpty ? 'EasyNotes reminder' : title,
        );
      } on PlatformException {
        // Exact alarms need a separate Android system setting. An inexact
        // schedule is still materially better than silently losing a reminder.
        await _schedule(
          id: id,
          reminder: reminder,
          title: title == null || title.isEmpty ? 'EasyNotes reminder' : title,
          exact: false,
        );
      }
    }
  }

  /// Browsers and Linux cannot hand a future notification to the OS through
  /// this plugin. While the app is running, a timer can still show it; closing
  /// the tab or app loses the timer.
  Future<void> _reconcileInProcess(
    List<Reminder> reminders,
    Map<String, NoteSummary> noteById,
  ) async {
    for (final timer in _inProcessTimers.values) {
      timer.cancel();
    }
    _inProcessTimers.clear();
    final used = <int>{};
    for (final reminder in reminders) {
      final title = noteById[reminder.noteId]?.title.trim();
      _scheduleInProcess(
        reminder,
        _notificationId(reminder.id, used),
        title == null || title.isEmpty ? 'EasyNotes reminder' : title,
      );
    }
  }

  void _scheduleInProcess(Reminder reminder, int id, String title) {
    final due = _nextOccurrence(reminder, _location(reminder.timezone));
    if (due == null) return;
    final delay = due.difference(
      tz.TZDateTime.now(_location(reminder.timezone)),
    );
    _inProcessTimers[reminder.id] = Timer(
      delay.isNegative ? Duration.zero : delay,
      () async {
        await _notifications.show(
          id: id,
          title: title,
          body: 'Reminder',
          notificationDetails: kIsWeb
              ? null
              : const NotificationDetails(linux: LinuxNotificationDetails()),
          payload: '$_payloadPrefix${reminder.id}',
        );
        if (reminder.repeat != ReminderRepeat.never) {
          _scheduleInProcess(reminder, id, title);
        } else {
          _inProcessTimers.remove(reminder.id);
        }
      },
    );
  }

  Future<void> _schedule({
    required int id,
    required Reminder reminder,
    required String title,
    bool exact = true,
  }) async {
    final location = _location(reminder.timezone);
    final scheduled = _nextOccurrence(reminder, location);
    if (scheduled == null) {
      await _notifications.cancel(id: id);
      return;
    }
    await _notifications.zonedSchedule(
      id: id,
      title: title,
      body: 'Reminder',
      scheduledDate: scheduled,
      notificationDetails: const NotificationDetails(
        android: AndroidNotificationDetails(
          _channelId,
          'Reminders',
          channelDescription: 'EasyNotes time-based reminders',
          importance: Importance.high,
          priority: Priority.high,
        ),
        iOS: DarwinNotificationDetails(),
      ),
      payload: '$_payloadPrefix${reminder.id}',
      androidScheduleMode: exact
          ? AndroidScheduleMode.exactAllowWhileIdle
          : AndroidScheduleMode.inexactAllowWhileIdle,
      matchDateTimeComponents: switch (reminder.repeat) {
        ReminderRepeat.never => null,
        ReminderRepeat.daily => DateTimeComponents.time,
        ReminderRepeat.weekly => DateTimeComponents.dayOfWeekAndTime,
        ReminderRepeat.monthly => DateTimeComponents.dayOfMonthAndTime,
        ReminderRepeat.yearly => DateTimeComponents.dateAndTime,
      },
    );
  }

  tz.Location _location(String name) {
    try {
      return tz.getLocation(name);
    } on ArgumentError {
      return tz.local;
    }
  }

  tz.TZDateTime? _nextOccurrence(Reminder reminder, tz.Location location) {
    final first = tz.TZDateTime.from(reminder.fireAt, location);
    final now = tz.TZDateTime.now(location);
    if (reminder.repeat == ReminderRepeat.never) {
      return first.isAfter(now) ? first : null;
    }
    if (first.isAfter(now)) return first;
    // The match components above make the OS repeat it. It nevertheless needs
    // an initial future instance, including when a long-lived reminder is
    // first received by a fresh device.
    switch (reminder.repeat) {
      case ReminderRepeat.daily:
        var next = tz.TZDateTime(
          location,
          now.year,
          now.month,
          now.day,
          first.hour,
          first.minute,
        );
        if (!next.isAfter(now)) {
          next = tz.TZDateTime(
            location,
            now.year,
            now.month,
            now.day + 1,
            first.hour,
            first.minute,
          );
        }
        return next;
      case ReminderRepeat.weekly:
        final ahead = (first.weekday - now.weekday + 7) % 7;
        var next = tz.TZDateTime(
          location,
          now.year,
          now.month,
          now.day + ahead,
          first.hour,
          first.minute,
        );
        if (!next.isAfter(now)) {
          next = tz.TZDateTime(
            location,
            now.year,
            now.month,
            now.day + ahead + 7,
            first.hour,
            first.minute,
          );
        }
        return next;
      case ReminderRepeat.monthly:
        return _nextMonth(location, now, first.day, first.hour, first.minute);
      case ReminderRepeat.yearly:
        return _nextYear(
          location,
          now,
          first.month,
          first.day,
          first.hour,
          first.minute,
        );
      case ReminderRepeat.never:
        return null;
    }
  }

  tz.TZDateTime _nextMonth(
    tz.Location location,
    tz.TZDateTime now,
    int day,
    int hour,
    int minute,
  ) {
    var next = _atClampedDay(location, now.year, now.month, day, hour, minute);
    if (!next.isAfter(now)) {
      next = _atClampedDay(
        location,
        now.year,
        now.month + 1,
        day,
        hour,
        minute,
      );
    }
    return next;
  }

  tz.TZDateTime _nextYear(
    tz.Location location,
    tz.TZDateTime now,
    int month,
    int day,
    int hour,
    int minute,
  ) {
    var next = _atClampedDay(location, now.year, month, day, hour, minute);
    if (!next.isAfter(now)) {
      next = _atClampedDay(location, now.year + 1, month, day, hour, minute);
    }
    return next;
  }

  tz.TZDateTime _atClampedDay(
    tz.Location location,
    int year,
    int month,
    int day,
    int hour,
    int minute,
  ) {
    final days = DateTime(year, month + 1, 0).day;
    return tz.TZDateTime(
      location,
      year,
      month,
      day.clamp(1, days),
      hour,
      minute,
    );
  }

  int _notificationId(String reminderId, Set<int> used) {
    var hash = 0x811c9dc5;
    for (final unit in reminderId.codeUnits) {
      hash = (hash ^ unit) * 0x01000193 & 0x7fffffff;
    }
    hash = hash == 0 ? 1 : hash;
    while (used.contains(hash)) {
      hash = hash == 0x7fffffff ? 1 : hash + 1;
    }
    return hash;
  }

  Future<void> dispose() async {
    await _updates?.cancel();
    for (final timer in _inProcessTimers.values) {
      timer.cancel();
    }
    _inProcessTimers.clear();
    if (identical(instance, this)) instance = null;
  }
}

/// The device's IANA timezone name, stored with the reminder so repeat rules
/// retain their local wall-clock meaning through DST and device travel.
Future<String> deviceTimezone() async {
  try {
    return (await FlutterTimezone.getLocalTimezone()).identifier;
  } on Object {
    return 'UTC';
  }
}

const _channelId = 'reminders';
const _payloadPrefix = 'easynotes-reminder:';
