import 'dart:async';

import 'support/fake_note_core.dart';
import 'support/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/reminder_scheduler.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:timezone/timezone.dart' as tz;

// Drive reconciliation explicitly so the test controls the interleaving.
class QuietLibrary extends FakeNoteLibrary {
  QuietLibrary({super.notes});

  int reminderWatchCount = 0;

  @override
  Stream<List<Reminder>> watchReminders() {
    reminderWatchCount++;
    return const Stream.empty();
  }
}

void main() {
  setUpAll(AndroidFlutterLocalNotificationsPlugin.registerWith);
  test('disposing during startup never watches the retired library', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final entered = Completer<void>();
    final zone = Completer<String>();
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(
      const MethodChannel('flutter_timezone'),
      (_) async {
        entered.complete();
        return zone.future;
      },
    );

    final library = QuietLibrary();
    final scheduler = ReminderScheduler(library);
    final starting = scheduler.start();
    await entered.future;
    await scheduler.dispose();
    zone.complete('UTC');
    await starting;

    expect(library.reminderWatchCount, 0);
  });

  Map<int, Map<dynamic, dynamic>> mockNotifications() {
    final pending = <int, Map<dynamic, dynamic>>{};
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('dexterous.com/flutter/local_notifications'),
          (call) async {
            switch (call.method) {
              case 'initialize':
                return true;
              case 'pendingNotificationRequests':
                return <Map<String, Object?>>[];
              case 'zonedSchedule':
                final args = call.arguments as Map;
                pending[args['id'] as int] = args;
                return null;
              case 'cancel':
                pending.remove((call.arguments as Map)['id']);
                return null;
            }
            return null;
          },
        );
    return pending;
  }

  test('a fixed-offset device zone still schedules its reminders', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final pending = mockNotifications();
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(
      const MethodChannel('flutter_timezone'),
      (_) async => 'GMT+02:00',
    );
    final library = QuietLibrary();
    final scheduler = ReminderScheduler(library);
    addTearDown(scheduler.dispose);
    final note = await library.createNote();
    await library.createReminder(
      noteId: note,
      fireAt: DateTime.utc(2090, 1, 1, 10),
      repeat: ReminderRepeat.never,
      timezone: 'GMT+02:00',
    );

    await scheduler.start();

    expect(pending, hasLength(1));
    expect(
      pending.values.single['scheduledDateTime'],
      startsWith('2090-01-01T10:00:00'),
    );
  });

  test('a daily fixed-offset reminder keeps 12:00 local', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final pending = mockNotifications();
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(
      const MethodChannel('flutter_timezone'),
      (_) async => 'GMT+02:00',
    );
    final library = QuietLibrary();
    final scheduler = ReminderScheduler(library);
    addTearDown(scheduler.dispose);
    final note = await library.createNote();
    // 12:00 on the +02:00 device is 10:00Z; a past instant makes the repeat
    // roll forward to its next daily occurrence.
    await library.createReminder(
      noteId: note,
      fireAt: DateTime.utc(2020, 1, 1, 10),
      repeat: ReminderRepeat.daily,
      timezone: 'GMT+02:00',
    );

    await scheduler.start();

    final scheduled = pending.values.single;
    // The fallback is UTC, so the OS is handed 10:00Z — the requested 12:00
    // on a +02:00 device. The zone name keeps that fallback explicit.
    expect(scheduled['timeZoneName'], tz.UTC.name);
    expect(scheduled['scheduledDateTimeISO8601'], endsWith('T10:00:00.000Z'));
    expect(scheduled['matchDateTimeComponents'], DateTimeComponents.time.index);
  });

  test('a reminder keeps the note title off the lock screen', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final pending = mockNotifications();
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(
      const MethodChannel('flutter_timezone'),
      (_) async => 'UTC',
    );
    final library = QuietLibrary(
      notes: {'note-1': FakeNoteCore(title: 'Divorce lawyer')},
    );
    final scheduler = ReminderScheduler(library);
    addTearDown(scheduler.dispose);
    await library.createReminder(
      noteId: 'note-1',
      fireAt: DateTime.utc(2090, 1, 1, 10),
      repeat: ReminderRepeat.never,
      timezone: 'UTC',
    );

    await scheduler.start();

    final scheduled = pending.values.single;
    expect(scheduled['title'], 'EasyNotes reminder');
    expect(scheduled['body'], 'Reminder');
    final android = scheduled['platformSpecifics'] as Map;
    expect(android['visibility'], NotificationVisibility.private.index);
    // An install that already has the channel keeps its old settings unless
    // the schedule updates it; the default is createIfNotExists.
    expect(
      android['channelAction'],
      AndroidNotificationChannelAction.update.index,
    );
  });

  for (final delete in [true, false]) {
    test(
      'a concurrent ${delete ? 'delete' : 'edit'} wins over an older schedule',
      () async {
        debugDefaultTargetPlatformOverride = TargetPlatform.android;
        addTearDown(() => debugDefaultTargetPlatformOverride = null);
        final pending = <int, Map<dynamic, dynamic>>{};
        Completer<void>? blocked;
        Completer<void>? entered;
        final messenger =
            TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
        messenger.setMockMethodCallHandler(
          const MethodChannel('flutter_timezone'),
          (_) async => 'UTC',
        );
        messenger.setMockMethodCallHandler(
          const MethodChannel('dexterous.com/flutter/local_notifications'),
          (call) async {
            switch (call.method) {
              case 'initialize':
                return true;
              case 'pendingNotificationRequests':
                return [
                  for (final entry in pending.entries)
                    {
                      'id': entry.key,
                      'payload': entry.value['payload'],
                      'title': entry.value['title'],
                      'body': entry.value['body'],
                    },
                ];
              case 'zonedSchedule':
                final gate = blocked;
                blocked = null;
                if (gate != null) {
                  entered!.complete();
                  await gate.future;
                }
                final args = call.arguments as Map;
                pending[args['id'] as int] = args;
                return null;
              case 'cancel':
                pending.remove((call.arguments as Map)['id']);
                return null;
            }
            return null;
          },
        );
        final library = QuietLibrary();
        final scheduler = ReminderScheduler(library);
        addTearDown(scheduler.dispose);
        await scheduler.start();
        final note = await library.createNote();
        final first = DateTime.utc(2090, 1, 1, 10);
        final id = await library.createReminder(
          noteId: note,
          fireAt: first,
          repeat: ReminderRepeat.never,
          timezone: 'UTC',
        );
        final gate = Completer<void>();
        blocked = gate;
        entered = Completer<void>();
        final oldRun = scheduler.reconcile();
        await entered.future;
        if (delete) {
          await library.forgetReminder(id);
        } else {
          await library.setReminder(
            Reminder(
              id: id,
              noteId: note,
              fireAt: first.add(const Duration(days: 1)),
              repeat: ReminderRepeat.never,
              timezone: 'UTC',
            ),
          );
        }
        final newRun = scheduler.reconcile();
        // Let the newer run finish if it incorrectly runs independently.
        await Future<void>.delayed(Duration.zero);
        gate.complete();
        await Future.wait([oldRun, newRun]);
        if (delete) {
          expect(pending, isEmpty);
        } else {
          expect(pending, hasLength(1));
          expect(
            pending.values.single['scheduledDateTime'],
            startsWith('2090-01-02T10:00:00'),
          );
        }
      },
    );
  }
}
