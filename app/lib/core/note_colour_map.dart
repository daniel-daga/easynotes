/// The two palettes are the same list in two places, which is what keeps the
/// UI from importing anything generated. A colour the Rust side adds and this
/// one has not heard of would not compile, which is the point of writing it out
/// rather than going through an index.
library;

import 'note_view.dart';
import '../src/rust/api/notes.dart' as rust;

NoteColour? colourFromRust(rust.NoteColour? colour) => switch (colour) {
  null => null,
  rust.NoteColour.red => NoteColour.red,
  rust.NoteColour.orange => NoteColour.orange,
  rust.NoteColour.yellow => NoteColour.yellow,
  rust.NoteColour.green => NoteColour.green,
  rust.NoteColour.teal => NoteColour.teal,
  rust.NoteColour.blue => NoteColour.blue,
  rust.NoteColour.purple => NoteColour.purple,
  rust.NoteColour.pink => NoteColour.pink,
  rust.NoteColour.brown => NoteColour.brown,
  rust.NoteColour.grey => NoteColour.grey,
};

rust.NoteColour? colourToRust(NoteColour? colour) => switch (colour) {
  null => null,
  NoteColour.red => rust.NoteColour.red,
  NoteColour.orange => rust.NoteColour.orange,
  NoteColour.yellow => rust.NoteColour.yellow,
  NoteColour.green => rust.NoteColour.green,
  NoteColour.teal => rust.NoteColour.teal,
  NoteColour.blue => rust.NoteColour.blue,
  NoteColour.purple => rust.NoteColour.purple,
  NoteColour.pink => rust.NoteColour.pink,
  NoteColour.brown => rust.NoteColour.brown,
  NoteColour.grey => rust.NoteColour.grey,
};
