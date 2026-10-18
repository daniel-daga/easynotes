/// Attachments in the note editor and on the note list, against a faked core
/// and faked pickers. What the real core does with the bytes is held in Rust
/// and in `relay_sync_test.dart`; this is what a person sees and taps.
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:easynotes/core/attachment_files.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/ui/attachments.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Pickers that hand over what the test set, and remember what was saved.
class FakeFiles implements AttachmentFiles {
  List<PickedFile> images = const [];
  List<PickedFile> audio = const [];
  final saved = <String, Uint8List>{};
  final thumbnailed = <int>[];

  @override
  Future<List<PickedFile>> pickImages() async => images;

  @override
  Future<List<PickedFile>> pickAudio() async => audio;

  @override
  Future<bool> save(String name, String mime, Uint8List bytes) async {
    saved[name] = bytes;
    return true;
  }

  @override
  Future<Uint8List?> thumbnail(Uint8List image) async {
    thumbnailed.add(image.length);
    return Uint8List.fromList([0x89, 0x50, 0x4E, 0x47, 1, 2, 3]);
  }
}

Uint8List png(int len) {
  final bytes = Uint8List(len);
  bytes.setAll(0, [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
  return bytes;
}

Uint8List mp3(int len) {
  final bytes = Uint8List(len);
  bytes.setAll(0, [0x49, 0x44, 0x33, 4]);
  return bytes;
}

void main() {
  late FakeNoteLibrary library;
  late String id;
  late FakeFiles files;

  Future<void> pump(WidgetTester tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: NoteScreen(
          core: library.core(id),
          storage: FakeStorageHealth(),
          noteId: id,
          invites: library,
          library: library,
          files: files,
        ),
      ),
    );
    await tester.pumpAndSettle();
  }

  Future<void> attach(WidgetTester tester, Key choice) async {
    await tester.tap(find.byKey(const Key('add-attachment')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(choice));
    await tester.pumpAndSettle();
  }

  /// Made inside each test rather than in `setUp`: the fake library's
  /// streams belong to the zone that made them, and a widget test pumps only
  /// its own.
  Future<void> fresh() async {
    library = FakeNoteLibrary();
    id = await library.createNote(kind: NoteKind.text);
    files = FakeFiles();
  }

  testWidgets('chosen images go into the note and show above the title', (
    tester,
  ) async {
    await fresh();
    files.images = [
      PickedFile(name: 'kitchen.png', bytes: png(4000)),
      PickedFile(name: 'garden.png', bytes: png(5000)),
    ];
    await pump(tester);
    await attach(tester, const Key('attach-images'));

    final core = library.core(id);
    expect(core.edits, [
      'importAttachment(kitchen.png)',
      'importAttachment(garden.png)',
    ]);
    // A preview is made for each, from the bytes chosen.
    expect(files.thumbnailed, [4000, 5000]);
    final shown = core.current.attachments;
    expect(shown.map((a) => a.name), ['kitchen.png', 'garden.png']);
    for (final a in shown) {
      expect(find.byKey(ValueKey('attachment:${a.id}')), findsOneWidget);
    }
    expect(find.bySemanticsLabel('kitchen.png'), findsOneWidget);
  });

  testWidgets('cancelling the picker adds nothing', (tester) async {
    await fresh();
    await pump(tester);
    await attach(tester, const Key('attach-images'));
    expect(library.core(id).edits, isEmpty);
    expect(find.byType(AttachmentGallery), findsNothing);
  });

  testWidgets(
    'an import waiting to upload says so, and a failure offers Retry',
    (tester) async {
      await fresh();
      final core = library.core(id)..holdImports = true;
      files.images = [PickedFile(name: 'later.png', bytes: png(3000))];
      await pump(tester);
      await attach(tester, const Key('attach-images'));

      expect(core.current.attachments, isEmpty);
      expect(find.text('Waiting to upload'), findsOneWidget);

      final pending = (await core.pendingAttachments()).single;
      library.pushTransfers([
        AttachmentTransfer(
          note: id,
          attachment: pending.id,
          upload: true,
          thumbnail: false,
          state: TransferState.running,
          done: 1,
          of: 4,
        ),
      ]);
      // The push reaches the screen a frame after it is made: through the
      // library's stream, then the screen's setState.
      await tester.pump();
      await tester.pump();
      expect(find.text('Uploading'), findsOneWidget);

      library.pushTransfers([
        AttachmentTransfer(
          note: id,
          attachment: pending.id,
          upload: true,
          thumbnail: false,
          state: TransferState.failed,
          error: 'the relay has no room for this attachment',
        ),
      ]);
      await tester.pump();
      await tester.pump();
      expect(find.text('Couldn\'t upload'), findsOneWidget);
      await tester.tap(find.text('Retry'));
      expect(library.calls, contains('retryTransfers()'));

      // Published: the note has it and nothing is waiting.
      core.publishPending();
      library.pushTransfers(const []);
      await tester.pumpAndSettle();
      expect(core.current.attachments.single.name, 'later.png');
      expect(find.text('Waiting to upload'), findsNothing);
    },
  );

  testWidgets(
    'a sound file is a row under the note, and removing it asks first',
    (tester) async {
      await fresh();
      files.audio = [
        PickedFile(name: 'market directions.mp3', bytes: mp3(2048)),
      ];
      await pump(tester);
      await attach(tester, const Key('attach-audio'));

      final core = library.core(id);
      final audio = core.current.attachments.single;
      expect(audio.kind, AttachmentKind.audio);
      expect(find.text('market directions.mp3'), findsOneWidget);
      expect(find.text('2 KB'), findsOneWidget);

      await tester.tap(find.byKey(const Key('attachment-menu')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Remove attachment'));
      await tester.pumpAndSettle();
      expect(find.textContaining('for everyone on this note'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(core.current.attachments, hasLength(1));

      await tester.tap(find.byKey(const Key('attachment-menu')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Remove attachment'));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('confirm-remove-attachment')));
      await tester.pumpAndSettle();
      expect(core.current.attachments, isEmpty);
      expect(find.text('market directions.mp3'), findsNothing);
    },
  );

  testWidgets('a picture opens whole, and Save a copy saves its bytes', (
    tester,
  ) async {
    await fresh();
    final photo = png(6000);
    files.images = [PickedFile(name: 'kitchen.png', bytes: photo)];
    await pump(tester);
    await attach(tester, const Key('attach-images'));
    final attachment = library.core(id).current.attachments.single;

    await tester.tap(find.byKey(ValueKey('attachment:${attachment.id}')));
    await tester.pumpAndSettle();
    expect(find.byKey(ValueKey('viewer:${attachment.id}')), findsOneWidget);
    await tester.tap(find.byKey(const Key('attachment-save')));
    await tester.pumpAndSettle();
    expect(files.saved['kitchen.png'], photo);

    // Describe it, from the viewer's menu.
    await tester.tap(find.byKey(const Key('attachment-menu')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Describe'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('attachment-description')),
      'The kitchen, before',
    );
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(
      library.core(id).current.attachments.single.description,
      'The kitchen, before',
    );
  });

  testWidgets(
    'a card counts pictures and sound files, and search filters by them',
    (tester) async {
      await fresh();
      final core = library.core(id);
      await core.setTitle('Recipe');
      await core.importAttachment('cake.png', png(100));
      await core.importAttachment('cake 2.png', png(100));
      await core.importAttachment('how.mp3', mp3(100));
      final other = await library.createNote();
      await library.core(other).setTitle('Shopping');

      await tester.pumpWidget(
        MaterialApp(home: NoteListScreen(library: library)),
      );
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('media-count')), findsOneWidget);
      expect(
        find.bySemanticsLabel(RegExp('2 images, 1 sound file')),
        findsOneWidget,
      );

      await tester.tap(find.byKey(const Key('search-button')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('filter-type-images')));
      await tester.pumpAndSettle();
      expect(find.text('Recipe'), findsOneWidget);
      expect(find.text('Shopping'), findsNothing);
    },
  );

  testWidgets(
    'Move earlier and Move later reorder pictures, and the viewer follows',
    (tester) async {
      await fresh();
      files.images = [
        PickedFile(name: 'one.png', bytes: png(100)),
        PickedFile(name: 'two.png', bytes: png(100)),
      ];
      // A sound file between them in the note's order: moving a picture past
      // the other picture must step over it.
      await pump(tester);
      await attach(tester, const Key('attach-images'));
      final core = library.core(id);
      await core.importAttachment('between.mp3', mp3(100));
      await core.moveAttachment(core.current.attachments.last.id, 1);
      expect(core.current.attachments.map((a) => a.name), [
        'one.png',
        'between.mp3',
        'two.png',
      ]);
      await tester.pumpAndSettle();

      final two = core.current.attachments.last;
      await tester.tap(find.byKey(ValueKey('attachment:${two.id}')));
      await tester.pumpAndSettle();
      expect(find.text('2 of 2'), findsOneWidget);
      await tester.tap(find.byKey(const Key('attachment-menu')));
      await tester.pumpAndSettle();
      expect(find.text('Move later'), findsNothing);
      await tester.tap(find.text('Move earlier'));
      await tester.pumpAndSettle();
      expect(core.current.attachments.map((a) => a.name), [
        'two.png',
        'one.png',
        'between.mp3',
      ]);
      // Still looking at the same picture, now first.
      expect(find.text('1 of 2'), findsOneWidget);
      expect(find.byKey(ValueKey('viewer:${two.id}')), findsOneWidget);

      await tester.tap(find.byKey(const Key('attachment-menu')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Move later'));
      await tester.pumpAndSettle();
      expect(
        core.current.attachments
            .map((a) => a.name)
            .where((n) => n.endsWith('.png')),
        ['one.png', 'two.png'],
      );
      expect(find.text('2 of 2'), findsOneWidget);
    },
  );

  testWidgets('a card shows the note\'s first picture above its title', (
    tester,
  ) async {
    await fresh();
    final core = library.core(id);
    await core.setTitle('Recipe');
    final picture = base64Decode(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgaPgPAAIDAYAkYfWXAAAAAElFTkSuQmCC',
    );
    await core.importAttachment('cake.png', png(100), thumbnail: picture);
    await core.importAttachment('how.mp3', mp3(100));
    await tester.pumpWidget(
      MaterialApp(home: NoteListScreen(library: library)),
    );
    await tester.pumpAndSettle();
    final cover = core.current.attachments.first;
    final header = find.byKey(ValueKey('cover:$id:${cover.id}'));
    void expectHeader() {
      final card = find.byKey(ValueKey('note:$id'));
      expect(header, findsOneWidget);
      expect(
        find.descendant(of: header, matching: find.byType(Image)),
        findsOneWidget,
      );
      expect(
        tester.getBottomLeft(header).dy,
        lessThan(tester.getTopLeft(find.descendant(
          of: card,
          matching: find.text('Recipe'),
        )).dy),
      );
      expect(find.descendant(of: card,
        matching: find.byIcon(Icons.audiotrack_outlined)), findsOneWidget);
    }
    expectHeader();
    await tester.tap(find.byKey(const Key('layout-toggle')));
    await tester.pumpAndSettle();
    expectHeader();
    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('search-field')), 'Recipe');
    await tester.pumpAndSettle();
    expectHeader();

    // Gone with the picture.
    await core.removeAttachment(cover.id);
    await tester.pumpAndSettle();
    expect(find.byKey(ValueKey('cover:$id:${cover.id}')), findsNothing);
  });
}

