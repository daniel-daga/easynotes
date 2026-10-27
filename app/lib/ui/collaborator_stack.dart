/// The people on a note, as a short stack of initials.
///
/// The note screen's Share action draws this in place of the person-plus icon
/// once somebody else is on the note, so who can open it is visible without
/// opening the sharing screen: up to [CollaboratorStack.maxVisible] overlapping
/// circles, the last one counting whoever did not fit.
///
/// This person is never drawn, somebody on their way out is not drawn, and
/// somebody who has been let in but whose access is still arriving is drawn
/// faded. Live presence — marking who has the note open right now — is deferred.
library;

import 'package:flutter/material.dart';

import '../core/invites.dart';

/// One circle in a [CollaboratorStack]: a person's initials.
@immutable
class CollaboratorCircle {
  const CollaboratorCircle({
    required this.initials,
    required this.seed,
    required this.name,
    this.pending = false,
  });

  final String initials;

  /// What decides the circle's colour: the member ID, so a person keeps the
  /// same circle wherever they appear.
  final String seed;

  /// For the stack's semantic label, and for saying who is still on their way.
  final String name;

  /// Let in, but not yet able to open the note. Drawn faded.
  final bool pending;

  @override
  bool operator ==(Object other) =>
      other is CollaboratorCircle &&
      other.initials == initials &&
      other.seed == seed &&
      other.name == name &&
      other.pending == pending;

  @override
  int get hashCode => Object.hash(initials, seed, name, pending);
}

class CollaboratorStack extends StatelessWidget {
  const CollaboratorStack({
    super.key,
    required this.people,
    this.ringColor,
    this.maxVisible = 3,
    this.radius = 12,
  }) : assert(maxVisible >= 1),
       assert(radius > 0);

  /// Everybody with access to the note, as the sharing screen lists them.
  final People people;

  /// What the circles are cut out of, normally the app bar behind them. Without
  /// it they are cut out of the theme's surface colour.
  final Color? ringColor;

  /// How many circles at most, counting the one that counts the rest.
  final int maxVisible;

  final double radius;

  /// Who the stack shows, in order: everybody on the note but this person,
  /// nobody on their way out, then the people whose access is still arriving.
  static List<CollaboratorCircle> entriesFor(People people) => [
    for (final person in people.people)
      if (!person.me && !person.removing)
        CollaboratorCircle(
          initials: person.initials,
          seed: person.member,
          name: person.displayName,
        ),
    for (final name in people.adding)
      CollaboratorCircle(
        initials: initialsFor(name),
        seed: 'adding:$name',
        name: name,
        pending: true,
      ),
  ];

  /// What a screen reader is told the stack shows.
  static String describe(List<CollaboratorCircle> circles) {
    final names = [for (final circle in circles) circle.name];
    final who = switch (names) {
      [] => '',
      [final only] => only,
      _ => '${names.take(names.length - 1).join(', ')} and ${names.last}',
    };
    return 'Shared with $who';
  }

  @override
  Widget build(BuildContext context) {
    final all = entriesFor(people);
    if (all.isEmpty) return const SizedBox.shrink();
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final ring = ringColor ?? scheme.surface;

    // The last circle counts the rest, so one fewer person is drawn.
    final room = all.length > maxVisible ? maxVisible - 1 : all.length;
    final shown = all.take(room).toList();
    final hidden = all.length - shown.length;
    final step = radius * 1.25;

    final circles = [
      for (final circle in shown)
        _circle(
          theme,
          circle,
          ring: ring,
          colours: _personColours(scheme, circle.seed),
        ),
      if (hidden > 0)
        _circle(
          theme,
          CollaboratorCircle(
            initials: '+$hidden',
            seed: 'more',
            name: '$hidden more',
          ),
          ring: ring,
          colours: (scheme.surfaceContainerHighest, scheme.onSurfaceVariant),
        ),
    ];

    final count = circles.length;
    return Semantics(
      label: describe(all),
      child: ExcludeSemantics(
        child: SizedBox(
          width: step * (count - 1) + radius * 2,
          height: radius * 2,
          child: Stack(
            clipBehavior: Clip.none,
            children: [
              // Right to left, so the first person's circle sits on top.
              for (var index = count - 1; index >= 0; index--)
                Positioned(left: index * step, child: circles[index]),
            ],
          ),
        ),
      ),
    );
  }

  Widget _circle(
    ThemeData theme,
    CollaboratorCircle circle, {
    required Color ring,
    required (Color, Color) colours,
  }) {
    final (background, foreground) = colours;
    final label = theme.textTheme.labelSmall!;
    final avatar = Container(
      width: radius * 2,
      height: radius * 2,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: background,
        shape: BoxShape.circle,
        border: Border.all(color: ring, width: 1.5),
      ),
      child: Text(
        circle.initials,
        // Sized to the circle rather than to a text role: the initials are
        // part of the control's geometry.
        style: label.copyWith(
          color: foreground,
          fontSize: radius * 0.85,
          fontWeight: FontWeight.w600,
          height: 1,
          letterSpacing: 0,
        ),
      ),
    );
    if (!circle.pending) return avatar;
    return Opacity(opacity: 0.45, child: avatar);
  }

  /// A background and the theme's own foreground for it, picked by the
  /// person's ID so one person keeps one circle. Three pairs, so a filled stack
  /// has three different circles; every pair is a theme role rather than an
  /// invented colour, which is what keeps the initials readable.
  (Color, Color) _personColours(ColorScheme scheme, String seed) {
    final pairs = [
      (scheme.primary, scheme.onPrimary),
      (scheme.tertiary, scheme.onTertiary),
      (scheme.secondary, scheme.onSecondary),
    ];
    var hash = 0;
    for (final rune in seed.runes) {
      hash = (hash * 31 + rune) & 0x7fffffff;
    }
    return pairs[hash % pairs.length];
  }
}
