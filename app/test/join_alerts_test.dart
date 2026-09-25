import 'package:easynotes/core/invites.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('the notification names the person and the note', () {
    final text = joinedNotificationText('Anna', 'Groceries');
    expect(text.title, 'Anna joined Groceries');
    expect(text.body, contains('can see and edit'));
  });

  test('an untitled note and an unnamed person still read as a sentence', () {
    expect(
      joinedNotificationText('Anna', '  ').title,
      'Anna joined a shared note',
    );
    expect(
      joinedNotificationText('', 'Groceries').title,
      'Someone joined Groceries',
    );
  });
}
