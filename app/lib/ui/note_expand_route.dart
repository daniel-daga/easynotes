/// The card-to-screen motion for opening a note.
///
/// A note is one surface in two places: a card on the list and the screen it
/// opens on. This route is what makes that true between the tap and the pop —
/// the tapped card grows until it is the screen, and shrinks back into the card
/// it came from, rather than one screen replacing another.
///
/// The trip follows Google Keep's container transform. The rectangle grows from
/// the card to the display along a slight arc, the corner radius eases away,
/// and two things cross-fade inside it: a copy of the card, at the size it was,
/// and the note screen, laid out at full size but scaled down to the rectangle's
/// width. Growing the card's *content* would make a small screen out of a large
/// one; scaling the screen down makes a small screen out of a screen.
///
/// Coming back is not the same trip played backwards. The note's contents go
/// first — within a frame or two — leaving a blank surface in the note's colour,
/// and that surface is what shrinks. The card fades in over the last fifth of
/// the way, so there is something to land on rather than a card that appears
/// once the surface is gone.
///
/// Android's back drag is previewed while it happens rather than played after
/// it: the finger pulls the whole note down to nine tenths of the display with
/// rounded corners, and holds it there. Only letting go turns that into the
/// close; letting go the other way puts the note back.
library;

import 'dart:ui' show lerpDouble;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show PredictiveBackEvent, SwipeEdge;

import 'motion.dart';

/// How wide the strip is, at the left edge, where an iOS swipe back begins.
const double _kBackGestureWidth = 20;

/// How much of the list, behind an opening note, is dimmed.
///
/// Keep's is about a third. Material's `OpenContainer` hard-codes `black54`,
/// which is too much: the note is meant to be the lit thing, not the only thing
/// left visible.
const double _kScrimOpacity = .30;

/// How far the note shrinks while a system back drag is in progress, and how
/// round its corners go. Android's predictive back previews the destination at
/// this size; Keep holds the note at the same place until the finger is lifted.
const double _kPreviewScale = .90;
const double _kPreviewRadius = 28;

/// Opens a note by growing [origin] — the card the person tapped, in screen
/// coordinates — until it covers the display.
///
/// The route paints the card's colour for the whole trip, draws a copy of the
/// card on top of it while the surface is still card-sized, and scales the
/// screen's content up inside it, so nothing is ever a small screen stretched
/// to a large one: the surface changes size, and what is on it arrives when
/// there is room.
///
/// Going back uses the same geometry, but not the same picture: the note's
/// contents leave at once, the blank surface left behind is what shrinks, and it
/// ends on the card *as it is then* — [resolveOrigin] is asked again, because the
/// note may have been edited, re-sorted, or the list scrolled while the screen
/// was open. A card that is no longer there falls back to the shared fade, which
/// is also what a note opened without a card ([origin] null: a share, a new
/// note, a link) uses.
class NoteExpandRoute<T> extends PageRoute<T> {
  NoteExpandRoute({
    required this.builder,
    required this.origin,
    required this.resolveOrigin,
    required this.startColor,
    required this.endColor,
    this.cardBuilder,
    this.onLanded,
    super.settings,
  }) : _begin = origin;

  final WidgetBuilder builder;

  /// The card's face, without its tap target: cover, title, preview, chips.
  ///
  /// Drawn inside the growing surface while it is still the size of a card, and
  /// faded out as the note arrives. Null when the note was not opened from a
  /// card, in which case the surface is plain colour until the note appears.
  final WidgetBuilder? cardBuilder;

  /// Called just before the shrinking surface lands on the card.
  ///
  /// The list uses it to put the real card back while the surface still covers
  /// it, so the hand-off from the copy inside the surface to the card on the
  /// list happens under something rather than in the open.
  final VoidCallback? onLanded;

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

  /// The display, as the last frame saw it. Kept because a close can begin from
  /// a gesture callback, outside a layout, and the rectangle it starts from has
  /// to be known right then.
  Rect _full = Rect.zero;

  /// The card's corner radius, from the theme, read each frame like the rest.
  double _corner = 16;

  // -- leaving ---------------------------------------------------------------

  /// Whether the surface is on its way to the card. While true the note's
  /// contents are not drawn at all: the surface shrinks as a blank sheet of the
  /// note's colour, and only the card reappears inside it near the end.
  bool _closing = false;

  /// The rectangle the close starts from. Full screen for a back button; the
  /// card-sized rectangle of a half finished swipe; the shrunken preview of a
  /// system back drag.
  Rect? _closeFrom;

  /// The controller value the close started at, so the rectangle's progress is
  /// measured from there rather than from a full-screen start it never had.
  double _closeStart = 1;

  /// The corner radius at [_closeFrom]: zero in full screen, [_kPreviewRadius]
  /// for a predictive-back preview.
  double _closeFromRadius = 0;

  /// Whether [onLanded] has already been told about this close.
  bool _landed = false;

  // -- predictive back -------------------------------------------------------

  /// How far a system back drag has pulled the note towards the preview, from
  /// 0 (full screen) to 1 (held at [_kPreviewScale]).
  ///
  /// The route's own controller stays at 1 while this runs: the drag is not yet
  /// a departure, and if it is let go the wrong way nothing should have to
  /// unwind. Only a commit turns into the real close.
  AnimationController? _preview;

  /// The edge the drag began at, so the preview leans away from the finger the
  /// way Android's own transition does.
  SwipeEdge _previewEdge = SwipeEdge.left;

  /// Whether the navigator has been told a user gesture is in progress and has
  /// not been told it ended.
  bool _userGesture = false;

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

  @override
  Duration get transitionDuration =>
      _instant ? Duration.zero : EasyMotion.expand;

  @override
  Duration get reverseTransitionDuration =>
      _instant ? Duration.zero : EasyMotion.collapse;

  @override
  AnimationController createAnimationController() {
    _preview = AnimationController(
      vsync: navigator!,
      duration: const Duration(milliseconds: 180),
    );
    return super.createAnimationController();
  }

  @override
  void dispose() {
    _stopUserGesture();
    _preview?.dispose();
    super.dispose();
  }

  // -- the trip --------------------------------------------------------------

  /// The screen's rectangle for an animation value, in screen coordinates.
  Rect _rectAt(double value) {
    final full = _full;
    final from = _begin ?? full;
    final preview = _preview?.value ?? 0;
    if (preview > 0) return _previewRect(full, preview);
    return _arc(from, full, EasyMotion.transform.transform(_clamped(value)));
  }

  /// Swaps a straight interpolation for one that arcs: the surface's top-left
  /// reaches the screen's corner first and the bottom-right lands last, which
  /// is the slight curve Keep's trip has and a plain [Rect.lerp] does not.
  Rect _arc(Rect begin, Rect end, double t) {
    if (t <= 0) return begin;
    if (t >= 1) return end;
    return MaterialRectArcTween(begin: begin, end: end).transform(t)!;
  }

  /// The note shrunk towards the middle while a system back drag is held.
  Rect _previewRect(Rect full, double progress) {
    final scale = lerpDouble(1, _kPreviewScale, progress)!;
    // Keep's preview leans away from the edge the finger came from, by about
    // Android's own eighth of a screen at most.
    final shift =
        (full.width / 20 - 8).clamp(0.0, 24.0) *
        progress *
        (_previewEdge == SwipeEdge.left ? 1 : -1);
    return Rect.fromCenter(
      center: full.center.translate(shift, 0),
      width: full.width * scale,
      height: full.height * scale,
    );
  }

  /// Where the note's contents are, against the trip's own progress. They
  /// arrive in the first half of the trip and are fully there by the time the
  /// surface is most of the screen: a line of text is never read while it is
  /// being resized.
  double _contentOpacity(double t) =>
      EasyMotion.enter.transform(((t - .15) / .4).clamp(0.0, 1.0));

  /// Where the card copy is, against the trip's own progress. It leaves a little
  /// faster than the note arrives, so the two are never equally lit.
  double _cardOpacity(double t) =>
      (1 - EasyMotion.exit.transform((t / .35).clamp(0.0, 1.0))).clamp(
        0.0,
        1.0,
      );

  void _beginClose() {
    if (_closing) return;
    final value = _clamped(controller?.value ?? 1);
    // Measure the rectangle before the card is re-measured, so the surface
    // starts where it is rather than jumping to a rectangle computed from where
    // the card has moved to since.
    final preview = _preview?.value ?? 0;
    _closeFrom = _rectAt(value);
    _closeFromRadius = preview > 0
        ? lerpDouble(0, _kPreviewRadius, preview)!
        : lerpDouble(_corner, 0, EasyMotion.transform.transform(value))!;
    _closeStart = value <= 0 ? 1 : value;
    _closing = true;
    _landed = false;
    _preview?.value = 0;
    _refreshBegin();
  }

  /// Re-measure the card before a departure, whether by button, system back or
  /// edge swipe.
  void _refreshBegin() => _begin = resolveOrigin() ?? _begin;

  @override
  bool didPop(T? result) {
    _beginClose();
    // The keyboard belongs to a screen that is leaving. Letting it start down
    // now rather than when the route is torn down means it is gone by the time
    // the card is back, instead of sliding back over it.
    FocusManager.instance.primaryFocus?.unfocus();
    return super.didPop(result);
  }

  @override
  Widget buildPage(
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
  ) => builder(context);

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
    _full = full;
    _corner = _cardRadius(theme);
    final progress = animation.drive(CurveTween(curve: EasyMotion.transform));
    final preview = _preview!;

    final transition = AnimatedBuilder(
      animation: Listenable.merge([animation, preview]),
      child: child,
      builder: (context, page) {
        // Read the card's rectangle every frame: [_refreshBegin] can move it
        // between frames when a departure re-measures the list.
        final from = _begin ?? full;
        final value = _clamped(animation.value);
        final pull = _clamped(preview.value);

        final Rect rect;
        final double radius;
        final Color colour;
        final double noteOpacity;
        final double cardOpacity;
        final double scrim;

        if (_closing) {
          // A blank sheet of the note's colour on its way to the card. The
          // contents left in the first frame; the card arrives in the last
          // fifth. Its progress is measured from where this close began, so a
          // dismissed half-swipe does not snap back to full screen first.
          final raw = _clamped((_closeStart - value) / _closeStart);
          final eased = EasyMotion.enter.transform(raw);
          rect = _arc(_closeFrom ?? full, from, eased);
          radius = lerpDouble(_closeFromRadius, _corner, eased)!;
          colour = Color.lerp(endColor, startColor, eased)!;
          noteOpacity = 0;
          // Measured against how far the surface has come, not against the
          // clock: it is nearly at the card for the last part of the time, and
          // a card that fades in on the clock would arrive after the landing.
          cardOpacity = eased < .80
              ? 0
              : EasyMotion.enter.transform(
                  ((eased - .80) / .20).clamp(0.0, 1.0),
                );
          scrim = 0;
          if (cardOpacity > 0 && !_landed && onLanded != null) {
            _landed = true;
            // Out of the build phase: the list this tells is an ancestor, and
            // marking it dirty here would be marking it dirty while it builds.
            WidgetsBinding.instance.addPostFrameCallback(
              (_) => onLanded?.call(),
            );
          }
        } else {
          final curved = progress.value.clamp(0.0, 1.0);
          var grown = _arc(from, full, curved);
          var corners = lerpDouble(_corner, 0, curved)!;
          var fill = Color.lerp(startColor, endColor, curved)!;
          if (pull > 0) {
            // A system back drag: the whole note is pulled in towards the
            // middle, still fully drawn, and no longer let through to anything
            // behind it.
            grown = Rect.lerp(grown, _previewRect(full, pull), pull)!;
            corners = lerpDouble(corners, _kPreviewRadius, pull)!;
            fill = Color.lerp(fill, endColor, pull)!;
          }
          rect = grown;
          radius = corners;
          colour = fill;
          // The fades follow the clock rather than the rectangle's curve: that
          // curve is most of the way there a third of the way through, and two
          // fades packed into the first eighth would be a cut, not a cross-fade.
          noteOpacity = _contentOpacity(value);
          cardOpacity = _cardOpacity(value) * (1 - pull);
          scrim = _kScrimOpacity * value * (1 - pull);
        }

        // Every layer is keyed, and stays keyed across the whole trip. The
        // scrim arrives on the second frame of the open and leaves on the first
        // frame of the close; unkeyed, that changes which slot the surface sits
        // in, and the surface is rebuilt in place as the thing that used to be
        // beside it. The note screen itself survives — Flutter re-adopts it
        // through a key of its own — but the surface and everything holding the
        // note would be destroyed and rebuilt, once per trip, on a frame in the
        // middle of the animation this is all for.
        return Stack(
          children: [
            // The list is still underneath, and dims as the note takes over.
            if (scrim > 0)
              Positioned.fill(
                key: const ValueKey('note-expanding-scrim-layer'),
                child: IgnorePointer(
                  child: ColoredBox(
                    key: const Key('note-expanding-scrim'),
                    color: Colors.black.withValues(alpha: scrim),
                  ),
                ),
              ),
            Positioned.fromRect(
              key: const ValueKey('note-expanding-surface-layer'),
              rect: rect,
              child: ClipRRect(
                key: const Key('note-expanding-surface'),
                borderRadius: BorderRadius.circular(radius),
                child: ColoredBox(
                  color: colour,
                  child: TickerMode(
                    // The note is invisible on the way out: nothing inside it —
                    // a blinking cursor, a progress ring — should keep waking
                    // the frames it is no longer seen in.
                    enabled: !_closing,
                    child: IgnorePointer(
                      // The note is invisible on the way out, and a screen that
                      // cannot be seen should not be able to be tapped or read.
                      ignoring: _closing,
                      child: ExcludeSemantics(
                        excluding: _closing,
                        child: Opacity(
                          key: const Key('note-expanding-content'),
                          opacity: noteOpacity.clamp(0.0, 1.0),
                          child: FittedBox(
                            // The note screen is laid out at its final size from
                            // the first frame; the growing rectangle only
                            // reveals it, at the rectangle's width rather than
                            // cropped to it. Since the rectangle keeps the
                            // display's aspect ratio, this is a uniform scale.
                            fit: BoxFit.fitWidth,
                            alignment: Alignment.topLeft,
                            child: SizedBox.fromSize(
                              key: const Key('note-expanding-page'),
                              size: full.size,
                              // Scaled, not laid out again: the screen keeps the
                              // same constraints all the way, so this tells the
                              // compositor to move a picture it already has
                              // rather than to draw a screen every frame.
                              child: RepaintBoundary(child: page),
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
            // The card, at the place and size it had when it was tapped, fading
            // out as the note arrives and back in just before the landing. It
            // is always inside the surface while it is drawn: it is the card's
            // own rectangle, and the surface starts and ends there.
            if (cardBuilder != null && cardOpacity > 0)
              Positioned.fromRect(
                key: const ValueKey('note-expanding-card-layer'),
                rect: from,
                child: IgnorePointer(
                  child: ExcludeSemantics(
                    child: Opacity(
                      key: const Key('note-card-copy'),
                      opacity: cardOpacity,
                      child: Builder(builder: cardBuilder!),
                    ),
                  ),
                ),
              ),
          ],
        );
      },
    );

    return _backGesture(
      context,
      navigator,
      _NotePredictiveBack(
        route: this,
        enabled: !reduceMotion,
        child: transition,
      ),
    );
  }

  // -- back gestures ---------------------------------------------------------

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
    if (dismiss) {
      _beginClose();
    } else {
      _closing = false;
      _closeFrom = null;
    }
    // The curve is applied where the rectangle is computed, not here: the
    // finger's own drag has no curve, and a settle that curved its value as
    // well would be curved twice.
    await (dismiss
        ? controller.animateBack(0, duration: duration)
        : controller.animateTo(1, duration: duration));
    return dismiss && controller.isDismissed;
  }

  bool get _instant => WidgetsBinding
      .instance
      .platformDispatcher
      .accessibilityFeatures
      .disableAnimations;

  void _startBackGesture(double progress, SwipeEdge edge) {
    _previewEdge = edge;
    _preview!
      ..stop()
      ..value = _clamped(progress);
    if (!_userGesture) {
      _userGesture = true;
      navigator?.didStartUserGesture();
    }
  }

  void _updateBackGesture(double progress) {
    _preview!.value = _clamped(progress);
  }

  /// The drag became a back. Pop now, from the preview's rectangle, rather than
  /// letting the system take over: the system's own transition would replace
  /// this surface with a page it knows nothing about.
  void _commitBackGesture() {
    if (!isCurrent || !popGestureEnabled) {
      _cancelBackGesture();
      return;
    }
    navigator?.pop();
    _endUserGestureWhenSettled();
  }

  /// The drag was let go before it was a back. The note returns to full screen
  /// and nothing else happens.
  void _cancelBackGesture() {
    final preview = _preview!;
    preview
        .animateTo(0, duration: EasyMotion.fast, curve: EasyMotion.enter)
        .whenComplete(_stopUserGesture);
  }

  /// The navigator is only told the drag is over once the route has stopped
  /// moving: while it animates, the gesture is still the thing driving it.
  void _endUserGestureWhenSettled() {
    final controller = this.controller;
    if (controller != null && controller.isAnimating) {
      void done(AnimationStatus status) {
        if (status.isAnimating) return;
        controller.removeStatusListener(done);
        _stopUserGesture();
      }

      controller.addStatusListener(done);
    } else {
      _stopUserGesture();
    }
  }

  void _stopUserGesture() {
    if (!_userGesture) return;
    _userGesture = false;
    navigator?.didStopUserGesture();
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

/// Listens for Android's predictive back drag and drives the note's preview.
///
/// The gesture never reaches a [PageRoute] by itself: Flutter offers it to every
/// [WidgetsBindingObserver], and the first one to say yes owns it. Claiming it
/// only when this route is the one on top, and only for a drag rather than a
/// button press, is what lets the back button keep working normally.
class _NotePredictiveBack<T> extends StatefulWidget {
  const _NotePredictiveBack({
    required this.route,
    required this.enabled,
    required this.child,
  });

  final NoteExpandRoute<T> route;

  /// Whether this trip is animated at all. With motion turned down there is no
  /// preview to hold, and claiming the gesture would only delay the pop.
  final bool enabled;
  final Widget child;

  @override
  State<_NotePredictiveBack<T>> createState() => _NotePredictiveBackState<T>();
}

class _NotePredictiveBackState<T> extends State<_NotePredictiveBack<T>>
    with WidgetsBindingObserver {
  bool get _enabled =>
      widget.enabled &&
      widget.route.isCurrent &&
      widget.route.popGestureEnabled;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  bool handleStartBackGesture(PredictiveBackEvent backEvent) {
    if (backEvent.isButtonEvent || !_enabled) return false;
    widget.route._startBackGesture(backEvent.progress, backEvent.swipeEdge);
    return true;
  }

  @override
  void handleUpdateBackGestureProgress(PredictiveBackEvent backEvent) {
    widget.route._updateBackGesture(backEvent.progress);
  }

  @override
  void handleCancelBackGesture() => widget.route._cancelBackGesture();

  @override
  void handleCommitBackGesture() => widget.route._commitBackGesture();

  @override
  Widget build(BuildContext context) => widget.child;
}

double _clamped(double value) => value.clamp(0.0, 1.0);

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
            dismiss ? EasyMotion.collapse : EasyMotion.expand,
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
