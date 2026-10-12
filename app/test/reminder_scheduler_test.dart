import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/reminder_scheduler.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:timezone/data/latest_all.dart' as data;
import 'package:timezone/timezone.dart' as tz;

void main() {
  setUpAll(data.initializeTimeZones);

  DateTime? next(
    DateTime first,
    ReminderRepeat repeat,
    DateTime now, {
    String zone = 'UTC',
  }) => ReminderScheduler.nextOccurrence(
    Reminder(
      id: 'r',
      noteId: 'n',
      fireAt: first,
      repeat: repeat,
      timezone: zone,
    ),
    tz.getLocation(zone),
    now: now,
  )?.toUtc();

  test('a monthly 31st skips February and retains March 31', () {
    final first = DateTime.utc(2026, 1, 31, 9);
    final march = next(first, ReminderRepeat.monthly, DateTime.utc(2026, 2, 1));
    expect(march, DateTime.utc(2026, 3, 31, 9));
    expect(
      next(first, ReminderRepeat.monthly, march!),
      DateTime.utc(2026, 5, 31, 9),
    );
  });

  test('a monthly 29th includes leap February and skips ordinary February', () {
    final first = DateTime.utc(2024, 1, 29, 9);
    expect(
      next(first, ReminderRepeat.monthly, DateTime.utc(2024, 2, 1)),
      DateTime.utc(2024, 2, 29, 9),
    );
    expect(
      next(first, ReminderRepeat.monthly, DateTime.utc(2025, 2, 1)),
      DateTime.utc(2025, 3, 29, 9),
    );
  });

  test(
    'yearly leap-day repeats retain February 29, including century boundaries',
    () {
      final first = DateTime.utc(2024, 2, 29, 9);
      expect(
        next(first, ReminderRepeat.yearly, DateTime.utc(2025)),
        DateTime.utc(2028, 2, 29, 9),
      );
      expect(
        next(first, ReminderRepeat.yearly, DateTime.utc(2097)),
        DateTime.utc(2104, 2, 29, 9),
      );
    },
  );

  test('month rollover and DST preserve local time', () {
    final location = tz.getLocation('Europe/Vienna');
    final first = tz.TZDateTime(location, 2025, 12, 31, 9);
    expect(
      next(
        first,
        ReminderRepeat.monthly,
        DateTime.utc(2026, 2, 1),
        zone: location.name,
      ),
      tz.TZDateTime(location, 2026, 3, 31, 9).toUtc(),
    );
  });

  test('one-shot expiration and future start dates are preserved', () {
    final first = DateTime.utc(2026, 5, 31, 9);
    expect(next(first, ReminderRepeat.never, first), isNull);
    expect(next(first, ReminderRepeat.monthly, DateTime.utc(2026, 1)), first);
  });

  test('watching reminders on a quiet library can be cancelled', () async {
    // Releasing a session cancels this subscription. It used to be an
    // `async*` loop over the note stream, whose cancel waited for a note
    // event that a quiet library never sends, and switching relay hung.
    final library = FakeNoteLibrary();
    final seen = <List<Reminder>>[];
    final watching = library.watchReminders().listen(seen.add);
    await pumpEventQueue();
    expect(seen, hasLength(1));
    await watching.cancel().timeout(const Duration(seconds: 1));
  });
}
