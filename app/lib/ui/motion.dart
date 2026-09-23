import 'package:flutter/material.dart';

/// The shared motion vocabulary for short, purposeful UI transitions.
abstract final class EasyMotion {
  static const fast = Duration(milliseconds: 180);
  static const medium = Duration(milliseconds: 300);
  static const enter = Curves.easeOutCubic;
  static const exit = Curves.easeInCubic;

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
