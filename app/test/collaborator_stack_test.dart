/// Widget tests for the app bar's stack of collaborator initials.
///
/// The stack answers one question at a glance: who else can open this note.
/// These check that it names the right people, never this person, and never
/// claims somebody has access before they do.
library;

import 'package:easynotes/core/invites.dart';
import 'package:easynotes/ui/collaborator_stack.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const me = Person(
  member: 'me',
  invite: '',
  name: 'Anna Berg',
  owner: true,
  me: true,
);

People sharedWith(List<Person> people, {List<String> adding = const []}) =>
    People(shared: true, canManage: true, people: people, adding: adding);

Future<void> pumpStack(WidgetTester tester, People people) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        appBar: AppBar(
          actions: [CollaboratorStack(people: people, ringColor: Colors.white)],
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('a note nobody else is on draws nothing', (tester) async {
    await pumpStack(tester, sharedWith(const [me]));

    expect(CollaboratorStack.entriesFor(sharedWith(const [me])), isEmpty);
    expect(tester.getSize(find.byType(CollaboratorStack)), Size.zero);
  });

  testWidgets('draws everybody but this person, and leaves out removals', (
    tester,
  ) async {
    await pumpStack(
      tester,
      sharedWith(const [
        me,
        Person(member: 'olga', invite: 'i1', name: 'Olga Petrov'),
        Person(member: 'rita', invite: 'i2', name: 'Rita Gone', removing: true),
        Person(member: 'ben', invite: 'i3', name: 'Ben Ash'),
      ]),
    );

    expect(find.text('OP'), findsOneWidget);
    expect(find.text('BA'), findsOneWidget);
    expect(find.text('AB'), findsNothing, reason: 'this person is not drawn');
    expect(find.text('RG'), findsNothing, reason: 'a removal is not access');
    expect(find.byType(Opacity), findsNothing);
  });

  testWidgets('somebody whose access is still arriving is drawn faded', (
    tester,
  ) async {
    await pumpStack(
      tester,
      sharedWith(const [me], adding: const ['Cara Waiting']),
    );

    expect(find.text('CW'), findsOneWidget);
    final fade = tester.widget<Opacity>(
      find.ancestor(of: find.text('CW'), matching: find.byType(Opacity)),
    );
    expect(fade.opacity, lessThan(1));
  });

  testWidgets('three circles at most, the last counting the rest', (
    tester,
  ) async {
    await pumpStack(
      tester,
      sharedWith(const [
        me,
        Person(member: 'a', invite: 'i1', name: 'A One'),
        Person(member: 'b', invite: 'i2', name: 'B Two'),
        Person(member: 'c', invite: 'i3', name: 'C Three'),
        Person(member: 'd', invite: 'i4', name: 'D Four'),
        Person(member: 'e', invite: 'i5', name: 'E Five'),
      ]),
    );

    expect(find.text('AO'), findsOneWidget);
    expect(find.text('BT'), findsOneWidget);
    expect(find.text('+3'), findsOneWidget);
    expect(
      find.descendant(
        of: find.byType(CollaboratorStack),
        matching: find.byType(Positioned),
      ),
      findsNWidgets(3),
    );
  });

  testWidgets('a screen reader is told who is on the note', (tester) async {
    final semantics = tester.ensureSemantics();
    await pumpStack(
      tester,
      sharedWith(const [
        me,
        Person(member: 'olga', invite: 'i1', name: 'Olga Petrov'),
        Person(member: 'ben', invite: 'i2', name: 'Ben Ash'),
      ]),
    );

    expect(
      find.bySemanticsLabel('Shared with Olga Petrov and Ben Ash'),
      findsOneWidget,
    );
    semantics.dispose();
  });
}
