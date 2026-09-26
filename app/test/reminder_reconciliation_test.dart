import 'dart:async';

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/reminder_scheduler.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_test/flutter_test.dart';

// Drive reconciliation explicitly so the test controls the interleaving.
class QuietLibrary extends FakeNoteLibrary {
  @override
  Stream<List<Reminder>> watchReminders() => const Stream.empty();
}

void main() {
  setUpAll(AndroidFlutterLocalNotificationsPlugin.registerWith);
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
