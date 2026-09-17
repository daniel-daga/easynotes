/// The Dart mirror of `easynotes_core::crdt::NoteView`.
///
/// These are plain immutable values. Nothing in the UI mutates them — a change
/// goes to the core as an edit and comes back as a whole new [NoteView] on the
/// watch stream. That is the rule the whole app follows, and building the first
/// screen around it is most of the point of this slice.
library;

import 'package:flutter/foundation.dart';

@immutable
class ItemView {
  const ItemView({
    required this.id,
    required this.text,
    required this.checked,
    required this.order,
  });

  /// The item's stable ID, base64url. Survives renames and reorders, which is
  /// what lets two devices agree about which item is which.
  final String id;
  final String text;
  final bool checked;

  /// The fractional index this item sorts by. The UI needs it only to work out
  /// where a drag landed.
  final String order;

  @override
  bool operator ==(Object other) =>
      other is ItemView &&
      other.id == id &&
      other.text == text &&
      other.checked == checked &&
      other.order == order;

  @override
  int get hashCode => Object.hash(id, text, checked, order);

  @override
  String toString() => 'ItemView($id, "$text", checked: $checked)';
}

@immutable
class NoteView {
  const NoteView({required this.title, required this.items});

  const NoteView.empty() : title = '', items = const [];

  final String title;

  /// Already in display order: the core sorts by `(order, id)` so that two
  /// devices showing the same data show it in the same sequence.
  final List<ItemView> items;

  int get checkedCount => items.where((i) => i.checked).length;

  @override
  bool operator ==(Object other) =>
      other is NoteView &&
      other.title == title &&
      listEquals(other.items, items);

  @override
  int get hashCode => Object.hash(title, Object.hashAll(items));
}
