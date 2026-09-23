import 'package:flutter/material.dart';

import '../core/note_library.dart';

/// The themed tint used for a note card and its detail view.
Color? noteColourTint(BuildContext context, NoteColour? colour) {
  if (colour == null) return null;
  final scheme = Theme.of(context).colorScheme;
  final base = switch (colour) {
    NoteColour.red => const Color(0xFFE53935),
    NoteColour.orange => const Color(0xFFF57C00),
    NoteColour.yellow => const Color(0xFFFDD835),
    NoteColour.green => const Color(0xFF43A047),
    NoteColour.teal => const Color(0xFF00897B),
    NoteColour.blue => const Color(0xFF1E88E5),
    NoteColour.purple => const Color(0xFF8E24AA),
    NoteColour.pink => const Color(0xFFD81B60),
    NoteColour.brown => const Color(0xFF6D4C41),
    NoteColour.grey => const Color(0xFF757575),
  };
  final strength = scheme.brightness == Brightness.dark ? 0.32 : 0.22;
  return Color.alphaBlend(base.withValues(alpha: strength), scheme.surface);
}
