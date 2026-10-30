/// The Dart mirror of `easynotes_core::crdt::NoteView`.
///
/// These are plain immutable values. Nothing in the UI mutates them — a change
/// goes to the core as an edit and comes back as a whole new [NoteView] on the
/// watch stream. That is the rule the whole app follows, and building the first
/// screen around it is most of the point of this slice.
library;

import 'package:flutter/foundation.dart';

import 'attachments.dart';

export 'attachments.dart';

@immutable
class ItemView {
  const ItemView({
    required this.id,
    required this.text,
    required this.checked,
    required this.order,
    this.heading = false,
  });

  /// The item's stable ID, base64url. Survives renames and reorders, which is
  /// what lets two devices agree about which item is which.
  final String id;
  final String text;
  final bool checked;

  /// The fractional index this item sorts by. The UI needs it only to work out
  /// where a drag landed.
  final String order;

  /// A section heading rather than something to tick off.
  ///
  /// A heading owns the rows under it until the next heading, so a section is a
  /// run of this ordered list and never a container of its own. It has no
  /// checkbox; a tick a device on an older build puts on it is merged and
  /// ignored ([ADR 0037](../../../docs/decisions.md#adr-0037)).
  final bool heading;

  @override
  bool operator ==(Object other) =>
      other is ItemView &&
      other.id == id &&
      other.text == text &&
      other.checked == checked &&
      other.order == order &&
      other.heading == heading;

  @override
  int get hashCode => Object.hash(id, text, checked, order, heading);

  @override
  String toString() =>
      'ItemView($id, "$text", checked: $checked, heading: $heading)';
}

/// Whether a note is prose or a list of things to tick off.
///
/// Shared, unlike a colour: it lives in the note's own document, so switching a
/// note switches it for everyone on it.
enum NoteKind { checklist, text }

/// A note's colour, as this person chose it.
///
/// A name rather than a `Color`, because both documents hold a name: what
/// "teal" has to look like is a question for the theme of whichever device is
/// drawing it, and the answer differs between daylight and dark mode. The map
/// from one to the other is in the note list screen, where the theme is.
enum NoteColour {
  red,
  orange,
  yellow,
  green,
  teal,
  blue,
  purple,
  pink,
  brown,
  grey,
}

@immutable
class NoteView {
  const NoteView({
    required this.title,
    required this.items,
    this.rev = 0,
    this.kind = NoteKind.checklist,
    this.body = '',
    this.colour,
    this.colourSet = false,
    this.attachments = const [],
  });

  const NoteView.empty()
    : title = '',
      items = const [],
      rev = 0,
      kind = NoteKind.checklist,
      body = '',
      colour = null,
      colourSet = false,
      attachments = const [];

  /// The revision this view was published at, strictly increasing for the life
  /// of an open note.
  ///
  /// It is what a shared field compares against its own last write: anything at
  /// or below that revision is a view the core had already published when the
  /// edit was applied — this device's own echo — and anything above it is a
  /// change somebody else made. Deliberately not part of [operator ==]: a view
  /// is the same view at a new revision, and treating a bump as new content
  /// would re-fire every listener for nothing. `0` is "this build has no
  /// revision for it", which for every view a fake makes is simply never newer
  /// than anything.
  final int rev;

  final NoteKind kind;
  final String title;

  /// The note's text. Present whatever [kind] says, as [items] are: a merge can
  /// leave content on the side a conversion emptied, and the rule is that a
  /// screen draws it rather than hiding it.
  final String body;

  /// The colour everyone on the note sees, or null. [colourSet] says whether
  /// the note has recorded one: until it has, a card shows this person's own
  /// colour from the account document, which is how colours chosen before
  /// colour was shared still show.
  final NoteColour? colour;
  final bool colourSet;

  /// Already in display order: the core sorts by `(order, id)` so that two
  /// devices showing the same data show it in the same sequence.
  final List<ItemView> items;

  /// Live attachments, in display order, whatever [kind] says. This device's
  /// imports that are not in the note yet are [NoteCore.pendingAttachments].
  final List<AttachmentView> attachments;

  int get checkedCount => items.where((i) => i.checked).length;

  @override
  bool operator ==(Object other) =>
      other is NoteView &&
      other.kind == kind &&
      other.title == title &&
      other.body == body &&
      other.colour == colour &&
      other.colourSet == colourSet &&
      listEquals(other.items, items) &&
      listEquals(other.attachments, attachments);

  @override
  int get hashCode => Object.hash(
    kind,
    title,
    body,
    colour,
    colourSet,
    Object.hashAll(items),
    Object.hashAll(attachments),
  );
}
