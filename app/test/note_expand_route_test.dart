/// The card-to-screen motion, as a person sees it: a tapped card grows until it
/// is the note, and every way back shrinks it into the card again.
///
/// The assertions are about rectangles on screen rather than about animation
/// objects, because the claim is about what is drawn: the surface the tap starts
/// is the card's, the surface it ends on is the display, the note inside it is
/// scaled rather than cropped, and the way back starts from the display and
/// lands on the card.
library;

import 'dart:async';

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/ui/note_expand_route.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
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
final page = find.byKey(const Key('note-expanding-page'));
final content = find.byKey(const Key('note-expanding-content'));
final cardCopy = find.byKey(const Key('note-card-copy'));
final scrim = find.byKey(const Key('note-expanding-scrim'));
final card = find.byKey(const Key('note:groceries'));
final cardCover = find.byKey(const Key('note-card-cover:groceries'));

double coverOpacity(WidgetTester tester) =>
    tester.widget<Opacity>(cardCover).opacity;

/// How lit the note's own screen is inside the growing surface.
double contentOpacity(WidgetTester tester) =>
    tester.widget<Opacity>(content).opacity;

double cardCopyOpacity(WidgetTester tester) =>
    tester.widget<Opacity>(cardCopy).opacity;

Size screen(WidgetTester tester) =>
    tester.view.physicalSize / tester.view.devicePixelRatio;

/// Whether the route showing the note is the growing one.
bool openedByExpanding(WidgetTester tester) =>
    ModalRoute.of(tester.element(find.byType(NoteScreen))) is NoteExpandRoute;

/// Sends one of Android's predictive-back events to the binding, the way the
/// engine does when a person drags in from the edge of the screen.
Future<void> backGesture(
  WidgetTester tester,
  String method, {
  double? progress,
}) async {
  await tester.binding.defaultBinaryMessenger.handlePlatformMessage(
    SystemChannels.backGesture.name,
    SystemChannels.backGesture.codec.encodeMethodCall(
      MethodCall(
        method,
        progress == null
            ? null
            : <String, Object?>{
                'touchOffset': <double>[0, 300],
                'progress': progress,
                'swipeEdge': SwipeEdge.left.index,
              },
      ),
    ),
    (_) {},
  );
  await tester.pump();
}

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

    // The first frame is the card, in the card's place — at the size it is
    // drawn while the press eases back out, a shade inside its box.
    expect(tester.getRect(surface).center, cardRect.center);
    expect(
      tester.getRect(surface).width,
      moreOrLessEquals(cardRect.width * .975, epsilon: 1),
    );
    expect(openedByExpanding(tester), isTrue);
    // The card itself is not drawn behind the screen that grew out of it...
    expect(coverOpacity(tester), 0);
    // ...but a copy of it is drawn inside the surface, so the thing growing is
    // the card rather than a flat colour.
    expect(cardCopy, findsOneWidget);
    expect(cardCopyOpacity(tester), moreOrLessEquals(1, epsilon: .01));
    // The note's own screen is already laid out at the display's size, scaled
    // down to the surface's width rather than cropped to it.
    expect(
      tester.getRect(page).width,
      moreOrLessEquals(tester.getRect(surface).width, epsilon: 1),
    );

    await tester.pump(const Duration(milliseconds: 30));
    // The card copy is on its way out while the note is still to arrive, and
    // what is left of it is filling the surface exactly, rather than sitting at
    // its old size inside a surface that has moved on: the two are one thing
    // growing.
    expect(cardCopyOpacity(tester), greaterThan(.5));
    expect(contentOpacity(tester), 0);
    expect(
      tester.getRect(cardCopy).width,
      moreOrLessEquals(tester.getRect(surface).width, epsilon: 1),
    );

    await tester.pump(const Duration(milliseconds: 90));
    final middle = tester.getRect(surface);
    expect(middle.width, greaterThan(cardRect.width));
    expect(middle.width, lessThan(screen(tester).width));
    expect(scrim, findsOneWidget);
    // The card has gone by now, and the note's own words are the only ones on
    // the surface: no ghost of both at once.
    expect(cardCopy, findsNothing);
    final arriving = contentOpacity(tester);
    expect(arriving, greaterThan(0));

    await tester.pump(const Duration(milliseconds: 60));
    expect(contentOpacity(tester), greaterThan(arriving));

    await tester.pumpAndSettle();
    expect(tester.getRect(surface), Offset.zero & screen(tester));
    expect(tester.getRect(page).width, moreOrLessEquals(screen(tester).width));
    expect(contentOpacity(tester), 1);
    expect(find.byType(NoteScreen), findsOneWidget);
  });

  testWidgets('the card is never gone while the surface is not there', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(card);
    // The navigator gives a new route its first frame a frame after the list
    // hears about it. Hiding the card on the tap left exactly one frame of
    // empty list between the card going and the card arriving.
    for (var frame = 0; frame < 6; frame++) {
      await tester.pump(const Duration(milliseconds: 16));
      expect(
        coverOpacity(tester) == 1 || surface.evaluate().isNotEmpty,
        isTrue,
        reason: 'frame $frame after the tap had neither card nor surface',
      );
    }
  });

  testWidgets('the surface grows into the whole screen at once', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(card);
    await tester.pump();
    await tester.pump();
    final start = tester.getRect(surface);
    final full = Offset.zero & screen(tester);

    double share(double value, double from, double to) =>
        (value - from) / (to - from);

    // Every edge is on the same curve, so the shape arrives whole. The rect
    // used to be a MaterialRectArcTween, whose corners travel along arcs: the
    // height was thirty points of the trip behind the width at every frame, and
    // finished after everything else had stopped.
    for (var frame = 0; frame < 14; frame++) {
      await tester.pump(const Duration(milliseconds: 16));
      final rect = tester.getRect(surface);
      final width = share(rect.width, start.width, full.width);
      final height = share(rect.height, start.height, full.height);
      expect(
        (width - height).abs(),
        lessThan(.02),
        reason: 'frame $frame: width is at $width, height at $height',
      );
    }
    await tester.pumpAndSettle();
    expect(tester.getRect(surface), full);
  });

  testWidgets('the surface rides the animation rather than being rebuilt', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(card);
    await tester.pump();
    await tester.pump();
    final opened = tester.element(surface);
    final note = tester.element(find.byType(NoteScreen));

    // The scrim arrives on the second frame of the open and leaves on the first
    // frame of the close. Without keys on the layers around the surface, both
    // of those frames threw the surface away and built a new one, taking the
    // note's ancestors with it.
    await tester.pump(const Duration(milliseconds: 60));
    expect(identical(tester.element(surface), opened), isTrue);
    await tester.pumpAndSettle();
    expect(identical(tester.element(surface), opened), isTrue);
    expect(identical(tester.element(find.byType(NoteScreen)), note), isTrue);

    await tester.pageBack();
    await tester.pump();
    await tester.pump();
    expect(identical(tester.element(surface), opened), isTrue);
    await tester.pump(const Duration(milliseconds: 100));
    expect(identical(tester.element(surface), opened), isTrue);
    expect(identical(tester.element(find.byType(NoteScreen)), note), isTrue);

    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsNothing);
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
    await tester.pump();

    // Nothing of the note is drawn on the way back: what shrinks is a blank
    // sheet in the note's colour.
    expect(contentOpacity(tester), 0);
    expect(scrim, findsNothing);
    expect(cardCopy, findsNothing);

    await tester.pump(const Duration(milliseconds: 120));
    final shrinking = tester.getRect(surface);
    expect(shrinking.width, lessThan(screen(tester).width));
    expect(shrinking.width, greaterThan(cardRect.width));

    // The card arrives inside the surface while there is still surface to
    // arrive in, rather than appearing when it goes — and it arrives filling
    // the surface, so the hand-over to the card on the list is a cross-fade
    // rather than a jump in size.
    await tester.pump(const Duration(milliseconds: 100));
    expect(cardCopy, findsOneWidget);
    expect(cardCopyOpacity(tester), greaterThan(0));
    expect(
      tester.getRect(cardCopy).width,
      moreOrLessEquals(tester.getRect(surface).width, epsilon: 1),
    );

    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsNothing);
    expect(coverOpacity(tester), 1);
    // One open, one handle, let go of exactly once.
    expect(library.core('groceries').disposals, 1);
  });

  testWidgets('a note that answers in time is on screen from the first frame', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(card);
    await tester.pump();
    await tester.pump();

    // The core answered inside the wait, so the trip carries the real screen
    // rather than a placeholder that is replaced somewhere in the middle of it.
    expect(find.byType(NoteScreen), findsOneWidget);
    expect(find.byKey(const Key('note-opening')), findsNothing);
  });

  testWidgets('a second tap while a note is opening opens one note', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    final gate = Completer<void>();
    library.openGate = gate;

    await tester.tap(card);
    await tester.pump();
    await tester.tap(card);
    await tester.pump();

    gate.complete();
    await tester.pumpAndSettle();

    expect(find.byType(NoteScreen), findsOneWidget);
    expect(
      library.calls.where((call) => call == 'openNote(groceries)'),
      hasLength(1),
    );

    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(library.core('groceries').disposals, 1);
  });

  testWidgets('a note whose card has gone fades away instead of landing', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await tester.tap(card);
    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsOneWidget);
    final full = tester.getRect(surface);

    // The card leaves the list while its note is open: archived from another
    // device, say. There is nothing on the list to shrink into any more.
    await library.setArchived('groceries', true);
    await tester.pumpAndSettle();
    expect(card, findsNothing);

    await tester.pageBack();
    await tester.pump();
    await tester.pump();

    // The surface stays where it was — it does not dive at the gap the card
    // used to be in — and the fade is what takes it away.
    expect(tester.getRect(surface), full);
    await tester.pump(const Duration(milliseconds: 140));
    expect(tester.getRect(surface), full);
    final fading = tester.widget<Opacity>(
      find.byKey(const Key('note-expanding-surface-opacity')),
    );
    expect(fading.opacity, lessThan(1));
    expect(contentOpacity(tester), 0);

    await tester.pumpAndSettle();
    expect(find.byType(NoteScreen), findsNothing);
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
    // The tap waits two frames for the core, then opens without it.
    await tester.pump(const Duration(milliseconds: 40));
    await tester.pump();
    // The screen is there once the wait is over, with the way back.
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

  testWidgets(
    'a system back drag holds the note at ninety percent and lets go on release',
    (tester) async {
      final library = twoLists();
      await pumpList(tester, library);
      final cardRect = tester.getRect(card);

      await tester.tap(card);
      await tester.pumpAndSettle();
      final full = tester.getRect(surface);

      await backGesture(tester, 'startBackGesture', progress: 0);
      await backGesture(tester, 'updateBackGestureProgress', progress: .5);

      final held = tester.getRect(surface);
      expect(held.width, lessThan(full.width));
      expect(held.width, greaterThan(cardRect.width));
      // Still the note on screen, still fully drawn, and the list behind it
      // less dimmed than it was: this is a preview of leaving, not leaving.
      expect(contentOpacity(tester), 1);
      expect(tester.widget<ColoredBox>(scrim).color.a, lessThan(.3));

      await backGesture(tester, 'cancelBackGesture');
      await tester.pumpAndSettle();
      expect(tester.getRect(surface), full);
      expect(find.byType(NoteScreen), findsOneWidget);
      expect(library.core('groceries').disposals, 0);

      await backGesture(tester, 'startBackGesture', progress: 0);
      await backGesture(tester, 'updateBackGestureProgress', progress: 1);
      expect(
        tester.getRect(surface).width,
        moreOrLessEquals(full.width * .9, epsilon: 1),
      );

      await backGesture(tester, 'commitBackGesture');
      await tester.pumpAndSettle();

      expect(find.byType(NoteScreen), findsNothing);
      expect(coverOpacity(tester), 1);
      expect(library.core('groceries').disposals, 1);
    },
  );

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
      // The note is still drawn under the finger.
      expect(contentOpacity(tester), greaterThan(0));

      await gesture.moveBy(
        const Offset(240, 0),
        timeStamp: const Duration(milliseconds: 200),
      );
      await gesture.up();
      await tester.pump();
      await tester.pump();
      // Let go: the contents go at once and the rest is a blank sheet.
      expect(contentOpacity(tester), 0);

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
      expect(contentOpacity(tester), 1);
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
