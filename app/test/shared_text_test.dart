import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/shared_text.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('what a share carries', () {
    test('text and a subject become a body and a title', () {
      expect(
        sharedTextFrom(subject: ' Recipe ', text: ' flour\nsugar '),
        const SharedText(text: 'flour\nsugar', subject: 'Recipe'),
      );
    });

    test('nothing to say is not a note', () {
      expect(sharedTextFrom(subject: 'Title only', text: '  '), isNull);
      expect(sharedTextFrom(), isNull);
    });

    test('a URL is added once, and not when the text already has it', () {
      expect(
        sharedTextFrom(text: 'Look', url: 'https://a.example')!.text,
        'Look\nhttps://a.example',
      );
      expect(
        sharedTextFrom(
          text: 'Look at https://a.example',
          url: 'https://a.example',
        )!.text,
        'Look at https://a.example',
      );
    });

    test('a web share target arrives in the query', () {
      final uri = Uri.parse(
        'https://notes.example/?share-title=Hi&share-text=milk#join#abc',
      );
      expect(
        sharedTextFromUri(uri),
        const SharedText(text: 'milk', subject: 'Hi'),
      );
      expect(sharedTextFromUri(Uri.parse('https://notes.example/')), isNull);
    });
  });

  group('text or list', () {
    test('only task lines make a list', () {
      expect(
        looksLikeTaskList('- [ ] milk\n\n* [x] bread\n+ [X] eggs'),
        isTrue,
      );
      expect(looksLikeTaskList('[ ] milk\n[x] bread\n  - [ ] eggs'), isTrue);
      expect(looksLikeTaskList('- [ ] milk\nand also bread'), isFalse);
      expect(looksLikeTaskList('[x]bread'), isFalse);
      expect(looksLikeTaskList('- milk'), isFalse);
      expect(looksLikeTaskList(''), isFalse);
    });

    test('a heading is a list line, but headings alone are not a list', () {
      expect(
        looksLikeTaskList('# Dairy\n[ ] milk\n## Frozen\n[x] peas'),
        isTrue,
      );
      expect(looksLikeTaskList('# Dairy\n[ ] milk'), isTrue);
      expect(looksLikeTaskList('# Dairy\n## Frozen'), isFalse);
      expect(looksLikeTaskList('# Dairy\nCall Anna'), isFalse);
    });

    test('a shared list keeps its sections', () async {
      final library = FakeNoteLibrary();
      final id = await createNoteFromSharedText(
        library,
        const SharedText(text: '# Dairy\n[ ] milk\n# Frozen\n[x] peas'),
      );
      final view = library.core(id).current;
      expect(view.kind, NoteKind.checklist);
      expect(view.items.map((i) => (i.text, i.heading, i.checked)), [
        ('Dairy', true, false),
        ('milk', false, false),
        ('Frozen', true, false),
        ('peas', false, true),
      ]);
    });

    test('a shared list comes back a list, ticks and all', () async {
      final library = FakeNoteLibrary();
      final id = await createNoteFromSharedText(
        library,
        const SharedText(text: '[ ] milk\n- [x] bread', subject: 'Shop'),
      );
      final view = library.core(id).current;
      expect(view.kind, NoteKind.checklist);
      expect(view.title, 'Shop');
      expect(view.items.map((i) => (i.text, i.checked)), [
        ('milk', false),
        ('bread', true),
      ]);
    });

    test('anything else stays text, exactly as sent', () async {
      final library = FakeNoteLibrary();
      final id = await createNoteFromSharedText(
        library,
        const SharedText(text: 'Call Anna\n- [ ] about the car'),
      );
      final view = library.core(id).current;
      expect(view.kind, NoteKind.text);
      expect(view.title, isEmpty);
      expect(view.body, 'Call Anna\n- [ ] about the car');
    });
  });
}
