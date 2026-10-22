/// The card-to-screen motion for opening a note.
///
/// A note is one surface in two places: a card on the list and the screen it
/// opens on. This route is what makes that true between the tap and the pop —
/// the tapped card grows until it is the screen, and shrinks back into the card
/// it came from, rather than one screen replacing another.
library;

import 'dart:ui' show lerpDouble;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';

import 'motion.dart';

/// How wide the strip is, at the left edge, where an iOS swipe back begins.
const double _kBackGestureWidth = 20;

/// Opens a note by growing [origin] — the card the person tapped, in screen
/// coordinates — until it covers the display.
///
/// The route paints the card's colour for the whole trip and fades the screen's
/// content in over it, so nothing is ever a small screen stretched to a large
/// one: the surface changes size, and what is on it arrives when there is room.
///
/// Going back plays the same trip in reverse and ends on the card *as it is
/// then* — [_resolveOrigin] is asked again, because the note may have been
/// edited, re-sorted, or the list scrolled while the screen was open. A card
/// that is no longer there falls back to the shared fade, which is also what a
/// note opened without a card ([origin] null: a share, a new note, a link)
/// uses.
class NoteExpandRoute<T> extends PageRoute<T> {
  NoteExpandRoute({
    required this.builder,
    required this.origin,
    required this.resolveOrigin,
    required this.startColor,
    required this.endColor,
    super.settings,
  }) : _begin = origin;

  final WidgetBuilder builder;

  /// The card when it was tapped. Null when the note was not opened from one.
  final Rect? origin;

  /// Where that card is now, asked again whenever the screen starts leaving so
  /// the reverse trip lands on it rather than where it used to be.
  final Rect? Function() resolveOrigin;

  /// The colour the trip starts with: the card's.
  final Color startColor;

  /// ...and the colour it ends with: the note screen's.
  final Color endColor;

  /// The card as it was last measured, which the animation interpolates from.
  Rect? _begin;

  /// Re-measure the card before a departure, whether by button, system back or
  /// edge swipe.
  void _refreshBegin() => _begin = resolveOrigin() ?? _begin;

  /// The route sits over the list rather than replacing it: the list is what
  /// the shrinking surface reveals on the way back.
  @override
  bool get opaque => false;

  @override
  bool get maintainState => true;

  @override
  Color? get barrierColor => null;

  @override
  String? get barrierLabel => null;

  // -- the back gesture ------------------------------------------------------

  /// Whether an edge drag may start now. False while the screen is still
  /// opening, and while something on it — a half-typed list item — has said
  /// that leaving is not allowed yet.
  bool get _canStartBackGesture => popGestureEnabled;

  double get _gestureProgress => animation?.value ?? 1;

  void _setGestureProgress(double value) {
    controller?.value = value;
  }

  /// Finishes a drag: settle to [dismiss] (0) or back to the screen (1). The
  /// answer is whether the route should now be popped, which is only true once
  /// the controller has actually reached the card.
  Future<bool> _settleBackGesture({
    required bool dismiss,
    required Duration duration,
  }) async {
    final controller = this.controller;
    if (controller == null) return false;
    // `animateBack` rather than `animateTo(0)`: the latter reports itself as
    // completed even when it lands on zero, and a route is only finished when
    // its animation says it was dismissed.
    await (dismiss
        ? controller.animateBack(
            0,
            duration: duration,
            curve: EasyMotion.transform,
          )
        : controller.animateTo(
            1,
            duration: duration,
            curve: EasyMotion.transform,
          ));
    return dismiss && controller.isDismissed;
  }

  bool get _instant => WidgetsBinding
      .instance
      .platformDispatcher
      .accessibilityFeatures
      .disableAnimations;

  @override
  Duration get transitionDuration =>
      _instant ? Duration.zero : EasyMotion.expand;

  @override
  Duration get reverseTransitionDuration =>
      _instant ? Duration.zero : EasyMotion.medium;

  @override
  Widget buildPage(
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
  ) => builder(context);

  @override
  bool didPop(T? result) {
    _refreshBegin();
    return super.didPop(result);
  }

  @override
  Widget buildTransitions(
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
    Widget child,
  ) {
    final navigator = Navigator.of(context);
    final reduceMotion = MediaQuery.maybeDisableAnimationsOf(context) ?? false;
    final begin = _begin;
    if (begin == null || reduceMotion) {
      // Nothing to grow out of, or motion was asked against: no trip at all.
      return _backGesture(
        context,
        navigator,
        begin == null && !reduceMotion
            ? EasyMotion.fadeThrough(child, animation)
            : child,
      );
    }

    final full = Offset.zero & MediaQuery.sizeOf(context);
    final theme = Theme.of(context);
    final corner = _cardRadius(theme);
    final progress = animation.drive(CurveTween(curve: EasyMotion.transform));

    final transition = AnimatedBuilder(
      animation: animation,
      child: child,
      builder: (context, page) {
        // Read the card's rectangle every frame: [_refreshBegin] can move it
        // between frames when a departure re-measures the list.
        final from = _begin ?? full;
        final t = progress.value.clamp(0.0, 1.0);
        final rect = Rect.lerp(from, full, t)!;
        final radius = lerpDouble(corner, 0, t)!;
        final colour = Color.lerp(startColor, endColor, t)!;
        // The screen's own content waits until the surface is most of the way
        // there, so a line of text is never read while it is being resized.
        // Measured against the beat, not the surface's own curve: that curve
        // is most of the way there a third of the way through, and content
        // fading in on it would be read while the card was still growing.
        final content = EasyMotion.enter.transform(
          ((animation.value - .25) / .5).clamp(0.0, 1.0),
        );
        return Stack(
          children: [
            // The list is still underneath, and dims as the note takes over.
            Positioned.fill(
              child: ColoredBox(color: Colors.black.withValues(alpha: .12 * t)),
            ),
            Positioned.fromRect(
              rect: rect,
              child: ClipRRect(
                key: const Key('note-expanding-surface'),
                borderRadius: BorderRadius.circular(radius),
                child: ColoredBox(
                  color: colour,
                  child: OverflowBox(
                    // The note screen is laid out at its final size from the
                    // first frame; the growing rectangle only reveals it.
                    alignment: Alignment.topLeft,
                    minWidth: full.width,
                    maxWidth: full.width,
                    minHeight: full.height,
                    maxHeight: full.height,
                    child: Opacity(opacity: content, child: page),
                  ),
                ),
              ),
            ),
          ],
        );
      },
    );

    return _backGesture(context, navigator, transition);
  }

  /// The edge swipe, where the platform has one.
  ///
  /// Only iOS and macOS: Android's back is the system's, and a second way to
  /// leave would be one too many.
  Widget _backGesture(
    BuildContext context,
    NavigatorState navigator,
    Widget child,
  ) {
    final platform = Theme.of(context).platform;
    if (platform != TargetPlatform.iOS && platform != TargetPlatform.macOS) {
      return child;
    }
    return _NoteBackGesture<T>(route: this, navigator: navigator, child: child);
  }
}

double _cardRadius(ThemeData theme) {
  final shape = theme.cardTheme.shape;
  if (shape is RoundedRectangleBorder) {
    return shape.borderRadius.resolve(TextDirection.ltr).topLeft.x;
  }
  return 16;
}

/// Drags the note screen back into its card, following the finger.
///
/// The same animation the back button plays, driven by the gesture instead of
/// by a duration: the transition controller is the finger's position, which is
/// what makes the surface stay under the finger rather than finish on its own.
class _NoteBackGesture<T> extends StatefulWidget {
  const _NoteBackGesture({
    required this.route,
    required this.navigator,
    required this.child,
  });

  final NoteExpandRoute<T> route;
  final NavigatorState navigator;
  final Widget child;

  @override
  State<_NoteBackGesture<T>> createState() => _NoteBackGestureState<T>();
}

class _NoteBackGestureState<T> extends State<_NoteBackGesture<T>> {
  /// The controller value the drag started from, usually 1.
  double _from = 1;
  double _dragged = 0;
  bool _dragging = false;

  /// Whether the navigator was told a gesture is in progress and has not been
  /// told it ended: a drag that is let go, cancelled, or interrupted by the
  /// route going away all have to end it exactly once.
  bool _gesturing = false;

  @override
  void dispose() {
    _stopGesture();
    super.dispose();
  }

  void _stopGesture() {
    if (!_gesturing) return;
    _gesturing = false;
    widget.navigator.didStopUserGesture();
  }

  void _start(DragStartDetails details) {
    final route = widget.route;
    if (_dragging || !route._canStartBackGesture) return;
    route._refreshBegin();
    _from = route._gestureProgress;
    _dragged = 0;
    _dragging = true;
    _gesturing = true;
    widget.navigator.didStartUserGesture();
  }

  void _update(DragUpdateDetails details) {
    if (!_dragging) return;
    _dragged = (_dragged + details.delta.dx).clamp(0.0, double.infinity);
    final width = context.size?.width ?? MediaQuery.sizeOf(context).width;
    if (width <= 0) return;
    widget.route._setGestureProgress(
      (_from - _dragged / width).clamp(0.0, 1.0),
    );
  }

  void _end(DragEndDetails details) {
    if (!_dragging) return;
    _dragging = false;
    final velocity = details.primaryVelocity ?? 0;
    // A flick either way wins over how far the finger got; otherwise half way
    // is the decision, as on every other screen that can be swiped away.
    final dismiss =
        velocity > 700 ||
        (velocity >= -700 && widget.route._gestureProgress < .5);
    widget.route
        ._settleBackGesture(
          dismiss: dismiss,
          duration: EasyMotion.duration(
            context,
            dismiss ? EasyMotion.medium : EasyMotion.expand,
          ),
        )
        .then((popped) {
          if (!mounted) return;
          _stopGesture();
          if (popped) widget.navigator.pop();
        });
  }

  void _cancel() {
    if (!_dragging) return;
    _dragging = false;
    widget.route
        ._settleBackGesture(
          dismiss: false,
          duration: EasyMotion.duration(context, EasyMotion.expand),
        )
        .whenComplete(() {
          if (mounted) _stopGesture();
        });
  }

  @override
  Widget build(BuildContext context) {
    final padding = MediaQuery.paddingOf(context);
    return Stack(
      children: [
        widget.child,
        Positioned(
          left: 0,
          top: 0,
          bottom: 0,
          width: _kBackGestureWidth + padding.left,
          child: GestureDetector(
            key: const Key('note-back-gesture'),
            behavior: HitTestBehavior.translucent,
            dragStartBehavior: DragStartBehavior.down,
            onHorizontalDragStart: _start,
            onHorizontalDragUpdate: _update,
            onHorizontalDragEnd: _end,
            onHorizontalDragCancel: _cancel,
          ),
        ),
      ],
    );
  }
}
