/// The card-to-screen motion, as a person sees it: a tapped card grows until it
/// is the note, and every way back shrinks it into the card again.
///
/// The assertions are about rectangles on screen rather than about animation
/// objects, because the claim is about what is drawn: the surface the tap starts
/// is the card's, the surface it ends on is the display, and the way back starts
/// from the display and lands on the card.
library;

import 'dart:async';

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/ui/note_expand_route.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

FakeNoteLibrary twoLists() => FakeNoteLibrary(
  notes: {
    'groceries': FakeNoteCore(
      title: 'Groceries',
      items: const ['milk', 'bread'],
    ),
    'hardware': FakeNoteCore(title: 'Hardware', items: const ['screws']),
  },
);

Future<void> pumpList(
  WidgetTester tester,
  FakeNoteLibrary library, {
  TargetPlatform platform = TargetPlatform.android,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(platform: platform),
      home: NoteListScreen(library: library),
    ),
  );
  await tester.pumpAndSettle();
}

final surface = find.byKey(const Key('note-expanding-surface'));
final card = find.byKey(const Key('note:groceries'));
final cardCover = find.byKey(const Key('note-card-cover:groceries'));

double coverOpacity(WidgetTester tester) =>
    tester.widget<Opacity>(cardCover).opacity;

Size screen(WidgetTester tester) =>
    tester.view.physicalSize / tester.view.devicePixelRatio;

/// Whether the route showing the note is the growing one.
bool openedByExpanding(WidgetTester tester) =>
    ModalRoute.of(tester.element(find.byType(NoteScreen))) is NoteExpandRoute;

void main() {
  testWidgets('a tapped card grows from its own rectangle to the screen', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    final cardRect = tester.getRect(card);

    await tester.tap(card);
    // One frame for the navigator to take the route, one for it to draw.
    await tester.pump();
    await tester.pump();

    // The first frame is the card, in the card's place.
    expect(tester.getRect(surface), cardRect);
    expect(openedByExpanding(tester), isTrue);
    // The card itself is not drawn behind the screen that grew out of it.
    expect(coverOpacity(tester), 0);

    await tester.pump(const Duration(milliseconds: 120));
    final middle = tester.getRect(surface);
    expect(middle.width, greaterThan(cardRect.width));
    expect(middle.width, lessThan(screen(tester).width));

    await tester.pumpAndSettle();
    expect(tester.getRect(surface), Offset.zero & screen(tester));
    expect(find.byType(NoteScreen), findsOneWidget);
  });

  testWidgets('the back button shrinks the screen into the card and lets go', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    final cardRect = tester.getRect(card);

    await tester.tap(card);
    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsOneWidget);

    await tester.pageBack();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 120));
    final shrinking = tester.getRect(surface);
    expect(shrinking.width, lessThan(screen(tester).width));
    expect(shrinking.width, greaterThan(cardRect.width));

    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsNothing);
    expect(coverOpacity(tester), 1);
    // One open, one handle, let go of exactly once.
    expect(library.core('groceries').disposals, 1);
  });

  testWidgets('a note closed before it finishes opening still lets go of it', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    final gate = Completer<void>();
    library.openGate = gate;

    await tester.tap(card);
    await tester.pump();
    await tester.pump();
    // The screen is there before the core has answered, with the way back.
    expect(find.byKey(const Key('note-opening')), findsOneWidget);
    expect(find.byType(NoteScreen), findsNothing);

    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note-opening')), findsNothing);

    gate.complete();
    await tester.pumpAndSettle();
    expect(library.core('groceries').disposals, 1);
    expect(coverOpacity(tester), 1);
  });

  testWidgets('a search result opens the same way as a card on the grid', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('search-field')), 'milk');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);

    await tester.tap(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    expect(openedByExpanding(tester), isTrue);
  });

  testWidgets('with motion turned down the note simply appears', (
    tester,
  ) async {
    final library = twoLists();
    await tester.pumpWidget(
      MaterialApp(
        builder: (context, child) => MediaQuery(
          data: MediaQuery.of(context).copyWith(disableAnimations: true),
          child: child!,
        ),
        home: NoteListScreen(library: library),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(card);
    await tester.pump();
    await tester.pump();
    expect(surface, findsNothing);
    expect(find.byType(NoteScreen), findsOneWidget);
  });

  group('the swipe back', () {
    testWidgets('is there on iOS, and dragging past half closes the note', (
      tester,
    ) async {
      final library = twoLists();
      await pumpList(tester, library, platform: TargetPlatform.iOS);
      final cardRect = tester.getRect(card);

      await tester.tap(card);
      await tester.pumpAndSettle();
      final full = tester.getRect(surface);
      expect(full.width, screen(tester).width);
      expect(find.byKey(const Key('note-back-gesture')), findsOneWidget);

      final gesture = await tester.startGesture(const Offset(4, 300));
      await gesture.moveBy(
        const Offset(240, 0),
        timeStamp: const Duration(milliseconds: 100),
      );
      await tester.pump();
      final dragged = tester.getRect(surface);
      expect(dragged.width, lessThan(full.width));
      expect(dragged.width, greaterThan(cardRect.width));

      await gesture.moveBy(
        const Offset(240, 0),
        timeStamp: const Duration(milliseconds: 200),
      );
      await gesture.up();
      await tester.pumpAndSettle();

      expect(find.byType(NoteScreen), findsNothing);
      expect(coverOpacity(tester), 1);
      expect(library.core('groceries').disposals, 1);
    });

    testWidgets('lets go before half way and the note stays open', (
      tester,
    ) async {
      final library = twoLists();
      await pumpList(tester, library, platform: TargetPlatform.iOS);
      await tester.tap(card);
      await tester.pumpAndSettle();
      final full = tester.getRect(surface);

      final gesture = await tester.startGesture(const Offset(4, 300));
      await gesture.moveBy(
        const Offset(80, 0),
        timeStamp: const Duration(milliseconds: 400),
      );
      await tester.pump();
      expect(tester.getRect(surface).width, lessThan(full.width));

      await gesture.up();
      await tester.pumpAndSettle();
      expect(find.byType(NoteScreen), findsOneWidget);
      expect(tester.getRect(surface), full);
      expect(library.core('groceries').disposals, 0);
    });

    testWidgets('is not on Android, where back is the system\'s', (
      tester,
    ) async {
      final library = twoLists();
      await pumpList(tester, library);
      await tester.tap(card);
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('note-back-gesture')), findsNothing);

      final gesture = await tester.startGesture(const Offset(4, 300));
      await gesture.moveBy(
        const Offset(400, 0),
        timeStamp: const Duration(milliseconds: 200),
      );
      await gesture.up();
      await tester.pumpAndSettle();
      expect(find.byType(NoteScreen), findsOneWidget);
    });
  });
}
