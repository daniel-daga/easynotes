/// Widget tests for the checklist screen, against a faked core.
///
/// These check one thing above all: that the screen is a function of the view it
/// is given. A gesture must produce an *edit* and nothing else, and the rendered
/// list must follow the stream even when the change did not originate here.
library;

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:easynotes/ui/checklist_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpScreen(
  WidgetTester tester,
  FakeNoteCore core, {
  Future<void> Function(String blob)? onJoin,
}) async {
  await tester.pumpWidget(
    MaterialApp(home: ChecklistScreen(core: core, onJoin: onJoin)),
  );
  await tester.pumpAndSettle();
}

List<String> visibleItems(WidgetTester tester) => tester
    .widgetList<TextField>(find.byType(TextField))
    .where(
      (f) => (f.key as ValueKey<String>?)?.value.startsWith('text:') ?? false,
    )
    .map((f) => f.controller!.text)
    .toList();

void main() {
  testWidgets('renders the title and items it is given', (tester) async {
    final core = FakeNoteCore(
      title: 'Groceries',
      items: const ['milk', 'bread', 'eggs'],
    );
    await pumpScreen(tester, core);

    expect(find.text('Groceries'), findsOneWidget);
    expect(visibleItems(tester), ['milk', 'bread', 'eggs']);
  });

  testWidgets('shows an empty state when there is nothing on the list', (
    tester,
  ) async {
    await pumpScreen(tester, FakeNoteCore());
    expect(find.byKey(const Key('empty-state')), findsOneWidget);
  });

  testWidgets('adding an item sends an edit and clears the field', (
    tester,
  ) async {
    final core = FakeNoteCore();
    await pumpScreen(tester, core);

    await tester.enterText(
      find.byKey(const Key('new-item-field')),
      'asparagus',
    );
    await tester.tap(find.byKey(const Key('add-item-button')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('addItem(asparagus)'));
    expect(visibleItems(tester), ['asparagus']);
    final field = tester.widget<TextField>(
      find.byKey(const Key('new-item-field')),
    );
    expect(
      field.controller!.text,
      isEmpty,
      reason: 'the field should be ready for the next item',
    );
  });

  testWidgets('a blank item is not added', (tester) async {
    final core = FakeNoteCore();
    await pumpScreen(tester, core);

    await tester.enterText(find.byKey(const Key('new-item-field')), '   ');
    await tester.tap(find.byKey(const Key('add-item-button')));
    await tester.pumpAndSettle();

    expect(core.edits, isEmpty);
  });

  testWidgets('checking an item sends an edit and strikes the text through', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final id = core.current.items[1].id;

    await tester.tap(find.byKey(Key('check:$id')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('setChecked($id, true)'));
    final text = tester.widget<TextField>(find.byKey(Key('text:$id')));
    expect(text.style?.decoration, TextDecoration.lineThrough);
  });

  testWidgets('editing an item commits when the field loses focus', (
    tester,
  ) async {
    // Committing per keystroke would put one update per character on the relay.
    final core = FakeNoteCore(items: const ['eggs']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    await tester.enterText(find.byKey(Key('text:$id')), 'eggs (12)');
    await tester.pump();
    expect(core.edits, isEmpty, reason: 'typing alone should not send an edit');

    // Move focus away.
    await tester.tap(find.byKey(const Key('new-item-field')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('setItemText($id, eggs (12))'));
  });

  testWidgets('deleting an item sends an edit', (tester) async {
    final core = FakeNoteCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    await tester.tap(find.byKey(Key('delete:$id')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('removeItem($id)'));
    expect(visibleItems(tester), ['bread']);
  });

  testWidgets('the share blob is shown so another device can join', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);

    await tester.tap(find.byKey(const Key('share-button')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('exportShareBlob()'));
    expect(find.byKey(const Key('share-blob')), findsOneWidget);
  });

  testWidgets('the screen follows the stream, not its own state', (
    tester,
  ) async {
    // This is the one that matters. Nothing was tapped: the change arrived the
    // way a collaborator's edit arrives from the relay, and the screen must
    // render it anyway.
    final core = FakeNoteCore(title: 'Groceries', items: const ['milk']);
    await pumpScreen(tester, core);
    expect(visibleItems(tester), ['milk']);

    core.pushRemote(
      const NoteView(
        title: 'Shopping',
        items: [
          ItemView(
            id: 'remote-1',
            text: 'coffee',
            checked: false,
            order: '0001',
          ),
          ItemView(
            id: 'remote-2',
            text: 'oat milk',
            checked: true,
            order: '0002',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['coffee', 'oat milk']);
    expect(find.text('Shopping'), findsOneWidget);
    expect(
      core.edits,
      isEmpty,
      reason: 'rendering a remote change must not echo an edit back',
    );
  });

  testWidgets('a remote rename replaces the text in an unfocused field', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['eggs']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    core.pushRemote(
      NoteView(
        title: core.current.title,
        items: [
          ItemView(id: id, text: 'eggs (12)', checked: false, order: '0000'),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['eggs (12)']);
  });

  testWidgets('dragging an item reports the index the core expects', (
    tester,
  ) async {
    // ReorderableListView counts the target index with the dragged item still in
    // place; the core wants it without. Getting this wrong moves items one slot
    // off, which is the kind of bug that only shows up in a demo.
    final core = FakeNoteCore(items: const ['one', 'two', 'three']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    final state = tester.state<State<ChecklistScreen>>(
      find.byType(ChecklistScreen),
    );
    final list = tester.widget<ReorderableListView>(
      find.byType(ReorderableListView),
    );
    expect(state, isNotNull);

    // Dragging item 0 down past item 2 arrives as (oldIndex: 0, newIndex: 2):
    // `onReorderItem` reports the target slot in the list without the dragged
    // item, which is the index the core wants.
    list.onReorderItem!(0, 2);
    await tester.pumpAndSettle();

    expect(core.edits, contains('moveItem($id, 2)'));
    expect(visibleItems(tester), ['two', 'three', 'one']);
  });

  testWidgets('dragging an item upwards passes the index through unchanged', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['one', 'two', 'three']);
    await pumpScreen(tester, core);
    final id = core.current.items[2].id;

    final list = tester.widget<ReorderableListView>(
      find.byType(ReorderableListView),
    );
    list.onReorderItem!(2, 0);
    await tester.pumpAndSettle();

    expect(core.edits, contains('moveItem($id, 0)'));
    expect(visibleItems(tester), ['three', 'one', 'two']);
  });

  // -- sync ------------------------------------------------------------------

  testWidgets('the sync button follows the status stream', (tester) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);

    // Nothing has connected yet.
    expect(
      tester.widget<IconButton>(find.byKey(const Key('sync-status'))).tooltip,
      'Not syncing',
    );

    core.pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pumpAndSettle();
    expect(
      tester.widget<IconButton>(find.byKey(const Key('sync-status'))).tooltip,
      'Up to date',
    );
  });

  testWidgets('waiting edits are counted where the user can see them', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);

    core.pushStatus(
      const SyncStatus(state: SyncState.offline, pending: 2),
    );
    await tester.pumpAndSettle();

    expect(
      tester.widget<IconButton>(find.byKey(const Key('sync-status'))).tooltip,
      'Not syncing, 2 edits waiting',
    );
    // The badge is the at-a-glance version of the same fact.
    expect(find.widgetWithText(Badge, '2'), findsOneWidget);
  });

  testWidgets('the relay\'s own words are available, not just ours', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);

    core.pushStatus(
      const SyncStatus(
        state: SyncState.offline,
        pending: 1,
        lastError: 'relay: Connection refused (os error 111)',
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();

    expect(find.text('Not syncing, 1 edit waiting'), findsOneWidget);
    expect(
      find.byKey(const Key('sync-error')),
      findsOneWidget,
      reason: 'a user who cannot sync should be able to see why',
    );
  });

  // -- joining ---------------------------------------------------------------

  testWidgets('pasting a share blob asks to join with it', (tester) async {
    String? joined;
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core, onJoin: (blob) async => joined = blob);

    await tester.tap(find.byKey(const Key('join-button')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('join-blob-field')),
      '  AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCk  ',
    );
    await tester.tap(find.byKey(const Key('join-confirm')));
    await tester.pumpAndSettle();

    expect(joined, 'AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCk');
  });

  testWidgets('an empty paste joins nothing', (tester) async {
    var calls = 0;
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core, onJoin: (blob) async => calls++);

    await tester.tap(find.byKey(const Key('join-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('join-confirm')));
    await tester.pumpAndSettle();

    expect(calls, 0);
  });

  testWidgets('there is no join button where joining is not possible', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);
    expect(find.byKey(const Key('join-button')), findsNothing);
  });
}
