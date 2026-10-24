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
/// *whether* to adopt is the field's business, because the body sends a `from`
/// and a `to` and an item sends only its text — but both decide it the same way,
/// with [isOwnEcho].
library;

import 'package:flutter/widgets.dart';

/// Whether a view is this field's own echo, and so has nothing to adopt.
///
/// [mine] is the newest revision the field has accounted for: the revision its
/// own last write landed at, or the revision of the newest view it has taken.
/// [text] is what that revision said, and [shown] is what the field is holding
/// for it.
///
/// Anything below [mine] was published before the field's last write, so it is
/// older than what the field already shows. A view at exactly [mine] is the
/// field's own write coming back — unless it says something else, which means
/// the core rebased that write onto somebody else's edit and the merge is what
/// to show and what the next send starts from.
bool isOwnEcho({
  required int rev,
  required int mine,
  required String text,
  required String shown,
}) => rev < mine || (rev == mine && text == shown);

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
