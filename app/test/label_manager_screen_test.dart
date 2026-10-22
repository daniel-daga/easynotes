import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/ui/label_manager_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpManager(
  WidgetTester tester,
  FakeNoteLibrary library, {
  String? noteId,
}) async {
  await tester.pumpWidget(
    MaterialApp(home: LabelManagerScreen(library: library, noteId: noteId)),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('creating labels does not create or label a note', (tester) async {
    final library = FakeNoteLibrary();
    await pumpManager(tester, library);
    await tester.enterText(find.byKey(const Key('label-field')), '  Travel  ');
    await tester.tap(find.byKey(const Key('label-add')));
    await tester.pumpAndSettle();

    expect(library.tags.single.name, 'Travel');
    expect(library.tags.single.notes, 0);
    expect(library.notes, isEmpty);
    expect(find.byKey(const Key('label-edit-Travel')), findsOneWidget);
    expect(
      tester.widget<TextField>(find.byKey(const Key('label-field')))
          .controller!.text,
      isEmpty,
    );
  });

  testWidgets('open manager follows remote creation, rename and deletion', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpManager(tester, library);
    final id = await library.makeTag('Travel');
    await tester.pumpAndSettle();
    expect(find.text('Travel'), findsOneWidget);
    await library.renameTag(id, 'Trips');
    await tester.pumpAndSettle();
    expect(find.text('Travel'), findsNothing);
    expect(find.text('Trips'), findsOneWidget);
    await library.forgetTag(id);
    await tester.pumpAndSettle();
    expect(find.text('Trips'), findsNothing);
    expect(find.text('No labels yet.'), findsOneWidget);
  });

  testWidgets('renaming onto an existing label preserves memberships', (
    tester,
  ) async {
    final library = FakeNoteLibrary(notes: {'note': FakeNoteCore()});
    await library.addTag('note', 'Travel');
    final survivor = await library.makeTag('Trips');
    await pumpManager(tester, library);
    await tester.tap(find.byKey(const Key('label-edit-Travel')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('rename-label-field')), 'Trips');
    await tester.tap(find.byKey(const Key('rename-label-confirm')));
    await tester.pumpAndSettle();

    expect(library.tags.single.id, survivor);
    expect(library.notes.single.tagIds, {survivor});
    expect(find.text('Merged "Travel" into "Trips".'), findsOneWidget);
  });

  testWidgets('note picker allows membership changes without label management', (
    tester,
  ) async {
    final library = FakeNoteLibrary(notes: {'note': FakeNoteCore()});
    final id = await library.makeTag('Travel');
    await pumpManager(tester, library, noteId: 'note');
    expect(find.byKey(const Key('label-search-field')), findsOneWidget);
    expect(find.byType(TextField), findsOneWidget);
    expect(find.byIcon(Icons.edit_outlined), findsNothing);
    expect(find.byKey(const Key('label-forget')), findsNothing);

    await tester.tap(find.byKey(const Key('label-check-Travel')));
    await tester.pumpAndSettle();
    expect(library.notes.single.tagIds, {id});
    await library.renameTag(id, 'Trips');
    await tester.pumpAndSettle();
    final check = find.byKey(const Key('label-check-Trips'));
    expect(tester.widget<CheckboxListTile>(check).value, isTrue);
    await tester.tap(check);
    await tester.pumpAndSettle();
    expect(library.notes.single.tags, isEmpty);
    expect(library.tags.single.name, 'Trips');
    await library.forgetTag(id);
    await tester.pumpAndSettle();
    expect(find.text('No labels yet.'), findsOneWidget);
  });

  testWidgets('cancelling deletion keeps the label and its notes', (tester) async {
    final library = FakeNoteLibrary(notes: {'note': FakeNoteCore()});
    await library.addTag('note', 'Travel');
    await pumpManager(tester, library);
    await tester.tap(find.byKey(const Key('label-edit-Travel')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-forget')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(library.tags.single.name, 'Travel');
    expect(library.notes.single.tags.single.name, 'Travel');
  });
}
