/// Text a second person can type into at the same time.
///
/// Two fields in a note are shared while they are being edited: the body, and
/// the text of a checklist item. Both are a [TextEditingController] that a
/// remote edit can arrive under, and both have to show the new text without
/// moving the caret somewhere else — replacing the whole value would put the
/// cursor at the end, mid-sentence, every time the other person typed.
///
/// The change is found as the one splice between the old text and the new, and
/// the selection is carried across it. That is all this file does; who decides
/// *whether* to adopt — a field's own echoes coming back, a send in flight — is
/// the field's business, because the body sends a `from` and a `to` and an item
/// sends only its text.
library;

import 'package:flutter/widgets.dart';

/// Show [text] in [controller], keeping the selection where it was relative to
/// the text around the change.
void adoptText(TextEditingController controller, String text) {
  final old = controller.text;
  if (text == old) return;
  final at = commonPrefix(old, text);
  final tail = commonSuffix(old, text, at);
  final oldEnd = old.length - tail;
  final newEnd = text.length - tail;
  int carry(int offset) {
    if (offset <= at) return offset;
    if (offset >= oldEnd) return offset - oldEnd + newEnd;
    return newEnd;
  }

  final selection = controller.selection;
  controller.value = TextEditingValue(
    text: text,
    selection: selection.isValid
        ? TextSelection(
            baseOffset: carry(selection.baseOffset),
            extentOffset: carry(selection.extentOffset),
          )
        : TextSelection.collapsed(offset: text.length),
  );
}

/// How much of a shared start [a] and [b] have, in UTF-16 code units.
int commonPrefix(String a, String b) {
  var i = 0;
  while (i < a.length && i < b.length && a.codeUnitAt(i) == b.codeUnitAt(i)) {
    i++;
  }
  return i;
}

/// How much of a shared end [a] and [b] have, beyond [prefix], in UTF-16 code
/// units.
int commonSuffix(String a, String b, int prefix) {
  var i = 0;
  while (i < a.length - prefix &&
      i < b.length - prefix &&
      a.codeUnitAt(a.length - 1 - i) == b.codeUnitAt(b.length - 1 - i)) {
    i++;
  }
  return i;
}
