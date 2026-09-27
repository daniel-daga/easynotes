/// The platform edge of reminders.
///
/// Rust owns encrypted reminder data and merge semantics. This file reads that
/// already-synced data and mirrors it into the operating system's local
/// scheduler; no reminder content or time reaches the relay as a consequence.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart' show PlatformException;
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_timezone/flutter_timezone.dart';
import 'package:timezone/data/latest_all.dart' as tz_data;
import 'package:timezone/timezone.dart' as tz;

import 'local_notifications.dart';
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
  Future<void>? _reconciling;
  bool _reconcileAgain = false;

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
    await initializeLocalNotifications();
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
  Future<void> reconcile() {
    if (!_started) return Future.value();
    // Platform calls yield. A second pass must read fresh state after the
    // current pass finishes, otherwise an older schedule can undo a deletion.
    // All callers await the same drain, including any requested follow-up.
    _reconcileAgain = true;
    return _reconciling ??= _drainReconciliations();
  }

  Future<void> _drainReconciliations() async {
    try {
      while (_started && _reconcileAgain) {
        _reconcileAgain = false;
        await _reconcileOnce();
      }
    } finally {
      _reconciling = null;
    }
  }

  Future<void> _reconcileOnce() async {
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
      final occurrence = nextOccurrence(reminder, _location(reminder.timezone));
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
    // Exact when the person has allowed it (Settings → Alarms & reminders,
    // which "Add reminder" opens); otherwise inexact, which Doze may delay by
    // some minutes. Null is iOS, where there is no such distinction.
    final exact =
        await _notifications
            .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin
            >()
            ?.canScheduleExactNotifications() ??
        true;
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
      final label = title == null || title.isEmpty
          ? 'EasyNotes reminder'
          : title;
      try {
        await _schedule(id: id, reminder: reminder, title: label, exact: exact);
      } on PlatformException {
        // Access withdrawn between the check and this call. A late reminder
        // is still materially better than a lost one.
        await _schedule(id: id, reminder: reminder, title: label, exact: false);
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
    final due = nextOccurrence(reminder, _location(reminder.timezone));
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
    required bool exact,
  }) async {
    final location = _location(reminder.timezone);
    final scheduled = nextOccurrence(reminder, location);
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

  /// Calendar repeats keep their original date, matching native notification
  /// components. Months without that day (and non-leap years for February 29)
  /// are skipped rather than changing every subsequent occurrence.
  @visibleForTesting
  static tz.TZDateTime? nextOccurrence(
    Reminder reminder,
    tz.Location location, {
    DateTime? now,
  }) {
    final first = tz.TZDateTime.from(reminder.fireAt, location);
    final current = now;
    final localNow = current == null
        ? tz.TZDateTime.now(location)
        : tz.TZDateTime.from(current, location);
    return _nextOccurrenceAt(reminder, location, first, localNow);
  }

  static tz.TZDateTime? _nextOccurrenceAt(
    Reminder reminder,
    tz.Location location,
    tz.TZDateTime first,
    tz.TZDateTime now,
  ) {
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

  static tz.TZDateTime _nextMonth(
    tz.Location location,
    tz.TZDateTime now,
    int day,
    int hour,
    int minute,
  ) {
    for (var month = now.month; ; month++) {
      final normalized = DateTime(now.year, month);
      if (day > DateTime(normalized.year, normalized.month + 1, 0).day) {
        continue;
      }
      final next = tz.TZDateTime(
        location,
        normalized.year,
        normalized.month,
        day,
        hour,
        minute,
      );
      if (next.isAfter(now)) return next;
    }
  }

  static tz.TZDateTime _nextYear(
    tz.Location location,
    tz.TZDateTime now,
    int month,
    int day,
    int hour,
    int minute,
  ) {
    for (var year = now.year; ; year++) {
      if (day > DateTime(year, month + 1, 0).day) continue;
      final next = tz.TZDateTime(location, year, month, day, hour, minute);
      if (next.isAfter(now)) return next;
    }
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

  Future<void> dispose({bool cancelScheduled = false}) async {
    _started = false;
    await _updates?.cancel();
    await _reconciling;
    for (final timer in _inProcessTimers.values) {
      timer.cancel();
    }
    _inProcessTimers.clear();
    if (cancelScheduled &&
        !kIsWeb &&
        defaultTargetPlatform != TargetPlatform.linux) {
      final pending = await _notifications.pendingNotificationRequests();
      for (final request in pending) {
        if (request.payload?.startsWith(_payloadPrefix) ?? false) {
          await _notifications.cancel(id: request.id);
        }
      }
    }
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
