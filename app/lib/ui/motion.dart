import 'package:flutter/material.dart';

/// The shared motion vocabulary for short, purposeful UI transitions.
abstract final class EasyMotion {
  static const fast = Duration(milliseconds: 180);
  static const medium = Duration(milliseconds: 300);

  /// A card growing into the screen. Longer than [medium] because it covers the
  /// whole display, and because the eye has to follow one surface all the way
  /// rather than two that cross. Most of that travel is over in the first half
  /// (see [transform]); the rest is the landing.
  static const expand = Duration(milliseconds: 260);

  /// A surface shrinking back into the thing it grew out of. Slightly shorter
  /// than [expand]: coming back is a dismissal, and a dismissal that takes as
  /// long as the arrival reads as hesitation.
  static const collapse = Duration(milliseconds: 240);

  static const enter = Curves.easeOutCubic;
  static const exit = Curves.easeInCubic;

  /// The curve for a surface that changes shape: off the card quickly, then a
  /// settle onto the screen, so it lands rather than stops.
  ///
  /// A quart, one step sharper than [enter]: a surface covering the whole
  /// display has to be most of the way there while the eye is still following
  /// it. A slower curve leaves the last few per cent of the screen growing for
  /// a tenth of a second after everything else has stopped, and that tail is
  /// what a trip like this is judged on — Google Keep is at 98% of the width
  /// within a fifth of its trip.
  static const transform = Curves.easeOutQuart;

  static Duration duration(BuildContext context, Duration preferred) =>
      MediaQuery.maybeOf(context)?.disableAnimations ?? false
      ? Duration.zero
      : preferred;

  static Widget fadeThrough(Widget child, Animation<double> animation) {
    final curved = CurvedAnimation(
      parent: animation,
      curve: enter,
      reverseCurve: exit,
    );
    return FadeTransition(
      opacity: curved,
      child: ScaleTransition(
        scale: Tween<double>(begin: .985, end: 1).animate(curved),
        child: child,
      ),
    );
  }
}

/// Brings replacement content in without retaining the outgoing subtree.
///
/// This is important for content backed by single-subscription streams: unlike
/// [AnimatedSwitcher], the outgoing screen is disposed before the new one is
/// mounted, so an animation can never listen to the same stream twice.
class EasyEntrance extends StatelessWidget {
  const EasyEntrance({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: EasyMotion.duration(context, EasyMotion.medium),
      curve: EasyMotion.enter,
      child: child,
      builder: (context, value, child) => Opacity(
        opacity: value,
        child: Transform.scale(
          scale: .985 + (.015 * value),
          alignment: Alignment.topCenter,
          child: child,
        ),
      ),
    );
  }
}
