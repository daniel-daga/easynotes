/// The screen the app opens on: every note this device holds.
///
/// It obeys the same rule as the checklist. There is one `StreamBuilder` over
/// the library's note list and everything below it is a function of what that
/// yields — a note created here, a note joined, a note someone else edited on
/// another device, and a note this person has just coloured or pinned all reach
/// the grid by the same path, because the core refreshes the list for all of
/// them.
///
/// The layout is Keep's: a masonry grid of cards, with a toggle to a single
/// column, and three shelves — the grid, the archive and the trash. Which
/// layout you chose is a view setting and which shelf a note is on is a
/// placement; both are personal, both live in the account document, and both
/// therefore outlive the screen.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/attachments.dart';
import '../core/note_core.dart';
import '../core/note_library.dart';
import '../core/recording_output.dart';
import '../core/reminder_scheduler.dart';
import '../core/relay_profiles.dart';
import '../core/shared_text.dart';
import '../core/sync_status.dart';
import 'about_screen.dart';
import 'audio_note_recorder.dart';
import 'attachments.dart';
import 'sync_devices_screen.dart';
import 'invites.dart';
import 'motion.dart';
import 'label_manager_screen.dart';
import 'note_appearance.dart';
import 'note_expand_route.dart';
import 'note_screen.dart';
import 'storage_banner.dart';
import 'sync_button.dart';

/// Where a note can be: in the grid, out of the way, or thrown away.
///
/// The list the library hands over holds all three, because filtering is a
/// question about a screen and not about a device. This is the filter.
enum _Shelf {
  notes('EasyNotes', 'No notes yet. Make one.'),
  archive('Archive', 'Nothing is archived.'),
  trash('Trash', 'The trash is empty.');

  const _Shelf(this.title, this.empty);

  final String title;
  final String empty;

  bool holds(NoteSummary note) => switch (this) {
    _Shelf.notes => note.inGrid,
    // The trash wins over the archive, so a note in both is only in the trash.
    _Shelf.archive => note.archived && !note.trashed,
    _Shelf.trash => note.trashed,
  };
}

enum _ReminderFilter { hasReminder, noReminder }

String _colourName(String value) => value == 'none'
    ? 'No colour'
    : '${value[0].toUpperCase()}${value.substring(1)}';

/// What the screen is showing: one of the three shelves, or one label.
///
/// A label is a fourth shelf rather than a filter inside the other three. That
/// is Keep's shape, and it is the one that needs no extra control: "the archived
/// notes labelled errands" is a question nobody has wanted a button for, and
/// giving a label its own shelf keeps the drawer a list of places rather than a
/// place and a modifier.
@immutable
class _Where {
  const _Where.shelf(this.shelf) : tag = null, tagName = null;

  /// A label's shelf is the grid, which is what makes a new note made here land
  /// somewhere this person can see it.
  const _Where.tag(String this.tag, String this.tagName) : shelf = _Shelf.notes;

  final _Shelf shelf;

  /// The label's **ID**, which is what membership is asked in and what survives
  /// the label being renamed.
  final String? tag;

  /// What that label was called when this shelf was opened. Drawn, never
  /// matched on — a rename refreshes it, and matching on it would lose the
  /// shelf the moment somebody renamed the label they were looking at.
  final String? tagName;

  String get title => tagName ?? shelf.title;

  String get empty => tag == null
      ? shelf.empty
      : 'Nothing here is labelled "${tagName ?? ''}".';

  /// Whether a new note made here would be visible afterwards. True on the grid
  /// and on a label — where the note is given that label, so it does not appear
  /// and then vanish.
  bool get takesNewNotes => shelf == _Shelf.notes;

  bool holds(NoteSummary note) => tag == null
      ? shelf.holds(note)
      // A note keeps its labels in the archive and in the trash; what a label
      // shows is what is on the shelf this person actually looks at.
      : note.inGrid && note.tagIds.contains(tag);

  @override
  bool operator ==(Object other) =>
      other is _Where && other.shelf == shelf && other.tag == tag;

  @override
  int get hashCode => Object.hash(shelf, tag);
}

class NoteListScreen extends StatefulWidget {
  NoteListScreen({
    super.key,
    required this.library,
    this.profile = const RelayProfile(id: 'default', url: null),
    RelayProfiles? profiles,
    this.onSwitchRelay = _ignoreRelaySwitch,
    this.onSetAccess,
    this.onMoveLocalToRelay,
    this.onAccountDeleted,
    this.initialInviteLink,
    this.onInviteLink,
    this.onInviteHandled,
    this.initialSharedText,
    this.sharedTexts,
  }) : profiles = profiles ?? RelayProfiles();

  final NoteLibrary library;
  final RelayProfile profile;
  final RelayProfiles profiles;
  final Future<void> Function(String?, {String? access}) onSwitchRelay;
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;

  /// Keep a new access key for the relay in use and reconnect with it. Null
  /// in tests that do not go there.
  final Future<void> Function(String?)? onSetAccess;

  /// Opens this profile again once its account is deleted. Null in tests that
  /// do not go there.
  final Future<void> Function()? onAccountDeleted;
  final String? initialInviteLink;

  /// A link that was pasted rather than tapped. The app takes it, puts this
  /// device on the link's relay if it names another one, and hands it back as
  /// [initialInviteLink] on the rebuilt list.
  final void Function(String link)? onInviteLink;

  /// The [initialInviteLink] has been joined or turned down, so it never needs
  /// showing again. The app holds it until then, so a rebuild on the way — a
  /// relay switch, an account being made — does not lose it.
  final void Function(String link)? onInviteHandled;

  /// Text another app shared in, which becomes a new note and opens: at
  /// launch, and then whenever the app is sent more while it runs.
  final SharedText? initialSharedText;
  final Stream<SharedText>? sharedTexts;

  // Standalone screen tests do not need profile management.
  static Future<void> _ignoreRelaySwitch(String? _, {String? access}) async {}

  @override
  State<NoteListScreen> createState() => _NoteListScreenState();
}

class _NoteListScreenState extends State<NoteListScreen> {
  /// What this person last chose, which the library remembers for them.
  ///
  /// Asked for on every build rather than read once, because it is in the
  /// account document and so another of this person's devices can change it.
  /// Nothing streams the layout on its own, and nothing needs to: a remote edit
  /// to the account document refreshes the note list, which rebuilds this
  /// screen. A copy taken when the screen opened was right until the first
  /// device was adopted, and then stayed wrong until a restart.
  NoteLayout get _layout => widget.library.layout;
  _Where _where = const _Where.shelf(_Shelf.notes);
  StreamSubscription<SharedText>? _shareSubscription;

  /// Whether the link this screen was given is already being joined. Guards the
  /// same link being started twice while its dialog is open.
  bool _joining = false;

  /// Watches the list for notes this device was just removed from, to say so
  /// once: “You no longer have access to ‘Groceries’.”
  StreamSubscription<List<NoteSummary>>? _removalSubscription;

  /// The note whose screen is open, if one is.
  ///
  /// Its card stays where it is but draws nothing: the screen opened from it is
  /// the same surface, and two of them on screen at once would be one too many.
  /// The card keeps its place in the layout so the trip back has somewhere to
  /// land, even after an edit re-sorted the list behind the open screen.
  final _expandedNoteId = ValueNotifier<String?>(null);

  /// The note whose core has been asked for and has not been pushed yet.
  ///
  /// Only set while [_openNote] is waiting for the core: the list is still
  /// touchable then, so a second tap on the card has to be told to wait rather
  /// than open the note twice.
  String? _openingNoteId;

  /// The render object of each card, so the expand route can ask where a card
  /// is — at the tap, and again when the screen starts shrinking back.
  final Map<String, GlobalKey> _cardKeys = {};

  /// The search box, when it is open, and what is in it.
  ///
  /// Not remembered the way the layout is: a colour is a decision about a note
  /// and a search is a question about right now, so it goes away with the
  /// screen. The query is held here as well as in the controller because the
  /// body is rebuilt from it and a controller is not a source of truth.
  final _search = TextEditingController();
  bool _searching = false;
  bool _addShortcutsExpanded = false;
  bool get _showFilters => _query.trim().isEmpty && _filter == null;

  bool _inSearchScope(NoteSummary note) =>
      _where.shelf == _Shelf.trash ? note.trashed : !note.trashed;
  String _query = '';
  // One selection across all categories; a new search cannot combine filters.
  ({String hint, bool Function(NoteSummary, Set<String>) matches})? _filter;

  /// The cards picked by a long press, by note ID.
  ///
  /// There is no separate "selecting" switch: the screen is selecting exactly
  /// when something is selected, so letting go of the last card is the way
  /// out as well as the close button. Replaced rather than changed in place,
  /// so the collections below can tell one selection from the next.
  Set<String> _selected = const {};
  bool get _selecting => _selected.isNotEmpty;

  /// Whether a note is on what this screen is showing right now — the shelf,
  /// or the search's scope — which is what a selection may hold.
  bool _inView(NoteSummary note) =>
      _searching ? _inSearchScope(note) : _where.holds(note);

  String get _searchHint =>
      _filter?.hint ??
      (_where.shelf == _Shelf.trash
          ? 'Search Trash'
          : 'Search notes and archive');

  @override
  void initState() {
    super.initState();
    _shareSubscription = widget.sharedTexts?.listen(_noteFromShare);
    _removalSubscription = widget.library.watchNotes().listen(
      (_) => _sayRemovals(),
    );
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final link = widget.initialInviteLink;
      if (link != null) _showJoinForLink(link);
      final shared = widget.initialSharedText;
      if (shared != null) unawaited(_noteFromShare(shared));
      _sayRemovals();
    });
  }

  /// The one message a removed person sees. The note has already left every
  /// device of theirs by the time it shows.
  void _sayRemovals() {
    if (!mounted) return;
    for (final removal in widget.library.takeRemovals()) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(
          key: Key('removed-${removal.noteId}'),
          content: Text(removal.message),
        ),
      );
    }
  }

  @override
  void dispose() {
    _shareSubscription?.cancel();
    _removalSubscription?.cancel();
    _search.dispose();
    _expandedNoteId.dispose();
    super.dispose();
  }

  GlobalKey _cardKeyFor(String id) => _cardKeys.putIfAbsent(id, GlobalKey.new);

  /// Where a card is on screen now, or null if it is not on screen at all.
  Rect? _cardRect(String id) {
    final object = _cardKeys[id]?.currentContext?.findRenderObject();
    if (object is! RenderBox || !object.attached) return null;
    return object.localToGlobal(Offset.zero) & object.size;
  }

  void _openSearch() => setState(() {
    _searching = true;
    _selected = const {};
  });

  void _closeSearch() {
    FocusManager.instance.primaryFocus?.unfocus();
    _search.clear();
    setState(() {
      _searching = false;
      _selected = const {};
      _where = const _Where.shelf(_Shelf.notes);
      _query = '';
      _filter = null;
    });
  }

  void _selectFilter(
    String hint,
    bool Function(NoteSummary, Set<String>) matches,
  ) {
    setState(() => _filter = (hint: hint, matches: matches));
  }

  bool _matchesFilter(NoteSummary note, Set<String> reminderNoteIds) =>
      _filter?.matches(note, reminderNoteIds) ?? true;

  void _resetSearch() {
    FocusManager.instance.primaryFocus?.unfocus();
    _search.clear();
    setState(() {
      _query = '';
      _filter = null;
    });
  }

  /// Open a note, and hold the handle open only while its screen is on top.
  ///
  /// The screen is pushed before the core has answered *for long*, rather than
  /// after: the card the person tapped is already growing into that space, and
  /// a tap that does nothing until a round trip finishes reads as a tap that
  /// missed. But a note that answers within [_kOpenWait] is worth waiting two
  /// frames for, because then the note screen is drawn from the first frame of
  /// the trip instead of being built in the middle of it — which is both a
  /// frame of work where the animation least wants one and, if it lands late
  /// enough, a body that pops into place under a surface that has already
  /// faded its contents in.
  ///
  /// The core's handle is let go of when the screen closes — including a screen
  /// that was closed again before the note opened, which would otherwise leak a
  /// Rust task per visit, for ever.
  ///
  /// [origin] is the card the tap landed on. Notes opened from anywhere else —
  /// a share, a link, the button that makes one — pass null and get the shared
  /// fade instead of growing from a card that is not there.
  Future<void> _openNote(
    String id, {
    bool autofocusBody = false,
    Rect? origin,
  }) async {
    // A tap that arrives while an earlier one is still waiting for the core is
    // the same tap twice: the card is still on the list, under nothing, so
    // nothing else stops it being pressed again.
    if (_openingNoteId != null) return;
    _openingNoteId = id;
    _clearSelection();
    final opening = widget.library.openNote(id);
    NoteCore? ready;
    try {
      ready = await opening.timeout(_kOpenWait);
    } on TimeoutException {
      // Still opening: the placeholder carries the trip and the real screen
      // arrives when it can.
    } on Object {
      // The screen says so itself, from the same future.
    }
    _openingNoteId = null;
    if (!mounted) {
      await _letGoOf(opening);
      return;
    }
    // Measured after the wait, not before it: the list may have moved under
    // the finger in the meantime, and the surface has to start where the card
    // is now — at the size it is drawn, which is still a hair inside its box
    // while the press is easing back out.
    final measured = origin == null ? null : _cardRect(id) ?? origin;
    final card = measured == null ? null : _asPressed(measured);
    final theme = Theme.of(context);
    final summary = widget.library.notes
        .where((note) => note.id == id)
        .firstOrNull;
    final tint = noteColourTint(context, summary?.colour);
    final cardColour =
        tint ?? theme.cardTheme.color ?? theme.colorScheme.surfaceContainerLow;
    final screenColour = tint ?? theme.scaffoldBackgroundColor;
    // The card as the list is drawing it, for the growing surface to carry
    // while it is still card-sized. Its cover is read through a scope on the
    // list, which the route is pushed above and therefore outside of: capture
    // the reader now, where there is one.
    final covers = _cardKeys[id]?.currentContext
        ?.getInheritedWidgetOfExactType<CoverScopeData>();
    final hasReminder = widget.library.reminders.any(
      (note) => note.noteId == id,
    );
    // A card with a cover can only be copied where its cover can be read.
    WidgetBuilder? cardCopy;
    if (card != null &&
        summary != null &&
        (summary.cover == null || covers != null)) {
      final copied = summary;
      cardCopy = (context) => _noteCardCopy(
        context,
        copied,
        hasReminder: hasReminder,
        covers: covers,
      );
    }
    final navigator = Navigator.of(context);
    final route = NoteExpandRoute<void>(
      settings: RouteSettings(name: 'note/$id'),
      origin: card,
      resolveOrigin: () => _cardRect(id),
      startColor: cardColour,
      endColor: screenColour,
      cardBuilder: cardCopy,
      cardSize: measured?.size,
      onLanded: () {
        // Put the real card back while the surface still covers it. Waiting
        // for the push to return would leave a frame with no card at all
        // between the surface going and the list drawing.
        if (mounted && _expandedNoteId.value == id) {
          _expandedNoteId.value = null;
        }
      },
      builder: (context) => _OpeningNote(
        opening: opening,
        ready: ready,
        colour: screenColour,
        builder: (core) => NoteScreen(
          core: core,
          storage: widget.library,
          library: widget.library,
          noteId: id,
          invites: widget.library,
          autofocusBody: autofocusBody,
          relayUrl: widget.profile.url,
          relayAccess: widget.profile.access,
          onLabels: () async {
            final note = widget.library.notes
                .where((note) => note.id == id)
                .firstOrNull;
            if (note != null) await _labelsFor(note);
          },
          onReminders: () async {
            final note = widget.library.notes
                .where((note) => note.id == id)
                .firstOrNull;
            if (note != null) await _remindersFor(note);
          },
        ),
      ),
    );
    // Hide the card once the surface that replaces it has been painted, and not
    // before: the navigator gives a new route its first frame one frame later
    // than the list notices anything, so hiding it here and now would put a
    // frame of empty list between the card going and the card arriving. The
    // growing surface starts as the card's own rectangle, so the card is
    // covered from that first frame and there is nothing to see underneath it.
    final pushed = navigator.push<void>(route);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _expandedNoteId.value = id;
    });
    try {
      await pushed;
    } finally {
      // The list can have gone while the note was on top — an account deleted
      // from under it, say — and its notifier with it.
      if (mounted && _expandedNoteId.value == id) {
        _expandedNoteId.value = null;
      }
      await _letGoOf(opening);
    }
  }

  /// Let go of the core a screen was opened for, whether it arrived or not.
  Future<void> _letGoOf(Future<NoteCore> opening) async {
    try {
      await (await opening).dispose();
    } on Object {
      // The screen has already said it could not open the note; there is no
      // handle to let go of.
    }
  }

  /// Make a note of what another app shared, and open it: on the main shelf,
  /// wherever the list happened to be, because a shared note is new and not
  /// filed yet.
  Future<void> _noteFromShare(SharedText shared) async {
    final String id;
    try {
      id = await createNoteFromSharedText(widget.library, shared);
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(content: Text('Could not make a note of that: $error')),
      );
      return;
    }
    if (!mounted) return;
    setState(() => _where = const _Where.shelf(_Shelf.notes));
    await _openNote(id);
  }

  /// A text note unless asked for a list, as in Keep: most quick notes are not
  /// lists, and one that turns out to be is a tap away.
  Future<void> _createNote({NoteKind kind = NoteKind.text}) async {
    final id = await widget.library.createNote(kind: kind);
    // Made while looking at a label, so it gets that label: otherwise the note
    // would be created, drawn for a frame, and gone from the shelf it was made
    // on as soon as the list refreshed.
    final tagName = _where.tagName;
    if (tagName != null) await widget.library.addTag(id, tagName);
    if (!mounted) return;
    await _openNote(id, autofocusBody: kind == NoteKind.text);
  }

  Future<void> _createAudioNote() async {
    setState(() => _addShortcutsExpanded = false);
    final recording = await showAudioNoteRecorder(context);
    if (recording == null) return;
    if (!mounted) {
      await discardRecordingPath(recording.path);
      return;
    }

    String? createdNoteId;
    try {
      final noteId = await widget.library.createNote(kind: NoteKind.text);
      createdNoteId = noteId;
      final tagName = _where.tagName;
      if (tagName != null) await widget.library.addTag(noteId, tagName);
      final core = await widget.library.openNote(noteId);
      try {
        await core.setTitle('Audio note');
        if (recording.bytes case final bytes?) {
          await core.importAttachment(
            'Voice note.wav',
            bytes,
            durationMs: recording.duration.inMilliseconds,
          );
        } else {
          await core.importAttachmentFile(
            recording.path,
            'Voice note.wav',
            durationMs: recording.duration.inMilliseconds,
          );
        }
      } finally {
        await core.dispose();
      }
      if (mounted) await _openNote(noteId);
    } on Object catch (error) {
      if (createdNoteId != null) {
        try {
          await widget.library.setTrashed(createdNoteId, true);
        } on Object {
          // Keep the original save error for the person.
        }
      }
      if (mounted) {
        ScaffoldMessenger.maybeOf(context)?.showSnackBar(
          SnackBar(content: Text('Could not save audio note: $error')),
        );
      }
    } finally {
      await discardRecordingPath(recording.path);
    }
  }

  Future<void> _manageLabels() => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (context) => LabelManagerScreen(library: widget.library),
    ),
  );

  /// Notes can choose existing labels; label management lives in the drawer.
  Future<void> _labelsFor(NoteSummary note) => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (context) =>
          LabelManagerScreen(library: widget.library, noteId: note.id),
    ),
  );

  /// The reminders attached to one note. They live in the library rather than
  /// in the card because they are account-document data and may arrive from a
  /// second device while this note is closed.
  Future<void> _remindersFor(NoteSummary note) => showDialog<void>(
    context: context,
    builder: (context) => _RemindersDialog(library: widget.library, note: note),
  );

  /// Show one label's notes, or go back to a shelf. Either way the drawer is the
  /// only way in, so it closes itself.
  ///
  /// A selection belongs to the shelf it was made on, so going somewhere else
  /// lets go of it; the same shelf under a new name (a renamed label) keeps it.
  void _show(_Where where) => setState(() {
    if (where != _where) _selected = const {};
    _where = where;
  });

  /// Ask to join a list by its invite link, and wait to be let in. A tapped
  /// link arrives as [initialLink] and is only confirmed; without one, the
  /// drawer's "Join with a link" asks for it to be pasted.
  ///
  /// A link names the relay it belongs to. When that is not this device's, the
  /// dialog hands it to the app ([onInviteLink]) instead of posting a request
  /// that server cannot see; the app moves this device over and hands the link
  /// back as [initialInviteLink], so nothing is pasted twice.
  ///
  /// The wait is a dialog because somebody who has just opened a link is
  /// usually on the phone to the person who sent it; closing it keeps the
  /// request, and [InviteBanners] keeps waiting where the dialog left off.
  Future<void> _join({String? initialLink}) async {
    // Joining needs an account and a name; the first time, this asks for them
    // and carries straight on.
    if (!await ensureSharingSetUp(context, widget.library) || !mounted) return;
    final join = await showDialog<PendingJoin>(
      context: context,
      builder: (context) => JoinDialog(
        invites: widget.library,
        initialLink: initialLink,
        profile: widget.profile,
        onHandOff: widget.onInviteLink,
      ),
    );
    if (join == null || !mounted) return;
    final id = await showDialog<String>(
      context: context,
      barrierDismissible: false,
      builder: (context) => WaitingDialog(invites: widget.library, join: join),
    );
    if (id == null || !mounted) return;
    await _openNote(id);
  }

  void _showJoinForLink(String link) {
    if (!mounted || _joining) return;
    _joining = true;
    unawaited(
      _join(initialLink: link).whenComplete(() {
        _joining = false;
        // Answered, joined, or turned down: either way this link is spent.
        widget.onInviteHandled?.call(link);
        // And if another one arrived while this dialog was up, it is next.
        final next = widget.initialInviteLink;
        if (mounted && next != null && next != link) _showJoinForLink(next);
      }),
    );
  }

  @override
  void didUpdateWidget(NoteListScreen old) {
    super.didUpdateWidget(old);
    // A new link while the list is already up — a warm launch, or a pasted
    // link the app has just settled on another relay.
    final link = widget.initialInviteLink;
    if (link != null && link != old.initialInviteLink) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _showJoinForLink(link);
      });
    }
  }

  /// Answer whoever asked to join one of this device's lists.
  Future<void> _review(String noteId) => showCollaborators(
    context,
    invites: widget.library,
    noteId: noteId,
    relayUrl: widget.profile.url,
    relayAccess: widget.profile.access,
  );

  /// A request that was waiting in the background was let in.
  void _joinedInBackground(String noteId) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        key: const Key('joined-in-background'),
        content: const Text('You can open the note now.'),
        action: SnackBarAction(
          label: 'Open',
          onPressed: () => _openNote(noteId),
        ),
      ),
    );
  }

  String _titleOf(String noteId) {
    final title = widget.library.notes
        .where((n) => n.id == noteId)
        .firstOrNull
        ?.title;
    return title == null || title.isEmpty ? 'a note' : '"$title"';
  }

  /// Where this device becomes one of a person's devices. Rebuilt on the way
  /// back, because the drawer's line about it has changed if an account was
  /// made or adopted and nothing streams that.
  Future<void> _openSyncDevices() async {
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (context) => SyncDevicesScreen(
          library: widget.library,
          profile: widget.profile,
          profiles: widget.profiles.profiles,
          onAccountDeleted: widget.onAccountDeleted,
          onMoveLocalToRelay: widget.onMoveLocalToRelay,
          onSetAccess: widget.onSetAccess,
          onSwitchRelay: widget.onSwitchRelay,
        ),
      ),
    );
    if (mounted) setState(() {});
  }

  Future<void> _openAbout() => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (context) => AboutScreen(profile: widget.profile),
    ),
  );

  Future<void> _toggleLayout() async {
    final next = _layout == NoteLayout.grid ? NoteLayout.list : NoteLayout.grid;
    await widget.library.setLayout(next);
    if (mounted) setState(() {});
  }

  // -- selecting cards ------------------------------------------------------
  //
  // A long press that goes nowhere picks a card up into a selection; one that
  // moves is a drag. While anything is selected a tap or a press adds or takes
  // away a card, nothing can be dragged, and the app bar becomes the actions
  // for the selection.

  /// A long press that did not turn into a drag.
  void _hold(NoteSummary note) {
    if (_selecting) {
      _toggle(note.id);
      return;
    }
    HapticFeedback.selectionClick();
    setState(() => _selected = {note.id});
  }

  void _toggle(String id) {
    HapticFeedback.selectionClick();
    setState(() {
      _selected = _selected.contains(id)
          ? ({..._selected}..remove(id))
          : {..._selected, id};
    });
  }

  void _clearSelection() {
    if (!_selecting || !mounted) return;
    setState(() => _selected = const {});
  }

  void _selectAll(List<NoteSummary> notes) =>
      setState(() => _selected = {for (final note in notes) note.id});

  /// The selection as notes, in the order they are drawn, without any that
  /// have left what is on screen since they were picked.
  List<NoteSummary> _selectedIn(List<NoteSummary> all) => [
    for (final note in all)
      if (_selected.contains(note.id) && _inView(note)) note,
  ];

  /// "Note" or "3 notes", for a sentence that starts with it.
  static String _counted(int count) => count == 1 ? 'Note' : '$count notes';

  /// Run a change to the selection, and say so if it did not happen.
  Future<bool> _change(Future<void> Function() action) async {
    try {
      await action();
      return true;
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.maybeOf(context)
          ?..hideCurrentSnackBar()
          ..showSnackBar(
            SnackBar(content: Text('Could not update the notes: $error')),
          );
      }
      return false;
    }
  }

  /// Say what was done to the selection, with a way to take it back.
  void _undoable(String what, Future<void> Function() undo) {
    if (!mounted) return;
    final messenger = ScaffoldMessenger.maybeOf(context);
    messenger?.hideCurrentSnackBar();
    messenger?.showSnackBar(
      SnackBar(
        key: const Key('selection-done'),
        content: Text(what),
        action: SnackBarAction(label: 'Undo', onPressed: () => _change(undo)),
      ),
    );
  }

  /// Move the selection between shelves: into or out of the archive, into or
  /// out of the trash.
  ///
  /// Going into either unpins, in the core, so the pins are written down here
  /// first: undoing has to put back what the person had, not what the core
  /// left.
  Future<void> _shelveSelected(
    List<NoteSummary> notes, {
    required Future<void> Function(List<String> ids, bool on) move,
    required bool on,
    required String done,
  }) async {
    final ids = [for (final note in notes) note.id];
    final pinned = [
      for (final note in notes)
        if (note.pinned) note.id,
    ];
    _clearSelection();
    if (!await _change(() => move(ids, on))) return;
    _undoable('${_counted(ids.length)} $done', () async {
      await move(ids, !on);
      if (pinned.isNotEmpty) await widget.library.setPinnedMany(pinned, true);
    });
  }

  Future<void> _archiveSelected(List<NoteSummary> notes, bool archived) =>
      _shelveSelected(
        notes,
        move: widget.library.setArchivedMany,
        on: archived,
        done: archived ? 'archived' : 'unarchived',
      );

  Future<void> _trashSelected(List<NoteSummary> notes, bool trashed) =>
      _shelveSelected(
        notes,
        move: widget.library.setTrashedMany,
        on: trashed,
        done: trashed ? 'moved to trash' : 'restored',
      );

  /// Pin the selection, or unpin it when every note in it is pinned already.
  Future<void> _pinSelected(List<NoteSummary> notes) async {
    final pin = notes.any((note) => !note.pinned);
    final ids = [for (final note in notes) note.id];
    _clearSelection();
    await _change(() => widget.library.setPinnedMany(ids, pin));
  }

  /// Colour the selection. The selection stays, so a colour that looked
  /// wrong on the cards can be changed again straight away.
  Future<void> _colourSelected(List<NoteSummary> notes) async {
    final colours = {for (final note in notes) note.colour};
    final chosen = await showModalBottomSheet<_Chosen<NoteColour?>>(
      context: context,
      builder: (context) => SafeArea(
        key: const Key('colour-sheet'),
        child: _ColourRow(
          // Marked only when the whole selection already shares it.
          chosen: colours.length == 1 ? colours.single : null,
          anyChosen: colours.length == 1,
          onChosen: (colour) => Navigator.of(context).pop(_Chosen(colour)),
        ),
      ),
    );
    if (chosen == null || !mounted) return;
    final ids = [for (final note in notes) note.id];
    await _change(() => widget.library.setColourMany(ids, chosen.value));
  }

  Future<void> _labelSelected(List<NoteSummary> notes) =>
      Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (context) => LabelManagerScreen(
            library: widget.library,
            noteIds: {for (final note in notes) note.id},
          ),
        ),
      );

  /// Delete the selection for good, if this person says so a second time.
  ///
  /// The confirmation is not ceremony. It is the only thing between a long press
  /// and lists nobody can get back, and it is where the promise the app cannot
  /// keep gets said out loud: deleting a shared list here does not delete it for
  /// whoever it was shared with, because their copy is theirs and under a key
  /// they hold.
  Future<void> _forgetSelected(List<NoteSummary> notes) async {
    final title = notes.length == 1 ? notes.single.title.trim() : '';
    final confirmed = await _confirmDelete(
      question: notes.length > 1
          ? 'Delete ${notes.length} notes permanently?'
          : title.isEmpty
          ? 'Delete this note permanently?'
          : 'Delete "$title" for good?',
      action: 'Delete',
    );
    if (!confirmed || !mounted) return;
    _clearSelection();
    try {
      await widget.library.forgetNotes([for (final note in notes) note.id]);
    } catch (error) {
      _deletionFailed('Could not finish deleting them: $error.');
    }
  }

  /// The same, for everything in the trash at once.
  Future<void> _emptyTrash(int count) async {
    final confirmed = await _confirmDelete(
      question: count == 1
          ? 'Delete 1 note permanently?'
          : 'Delete all $count notes permanently?',
      action: 'Delete all',
    );
    if (!confirmed || !mounted) return;
    try {
      await widget.library.emptyTrash();
    } catch (error) {
      _deletionFailed('Could not finish emptying the trash: $error.');
    }
  }

  /// A deletion that did not finish, said where the person is looking.
  ///
  /// Not an error thrown out of a button: that reaches nobody. Each note's
  /// deletion is recorded before anything of it is removed, so whatever is
  /// gone stays gone and whatever is still in the trash is whole. Deleting
  /// again is therefore always safe, and the message says so.
  void _deletionFailed(String what) {
    if (!mounted) return;
    // In front of whatever was being said, such as how to undo moving the
    // note to the trash: this is the one that needs reading now.
    ScaffoldMessenger.maybeOf(context)
      ?..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          key: const Key('deletion-failed'),
          content: Text(
            '$what Anything still in the trash is whole and can be deleted '
            'again.',
          ),
        ),
      );
  }

  Future<bool> _confirmDelete({
    required String question,
    required String action,
  }) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('confirm-delete'),
        title: Text(question),
        // Worded so that it reads the same for one list and for a trash full of
        // them, and so that the second sentence is there at all: a person
        // deleting a shared shopping list may well think they are deleting it
        // for the household, and they are not.
        content: const Text(
          'Deleted from your account. Your other devices will delete their '
          'copies when they sync. There is no undo. People you shared this '
          'note with keep their copies.',
        ),
        actions: [
          TextButton(
            key: const Key('confirm-keep'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Keep'),
          ),
          TextButton(
            key: const Key('confirm-delete-forever'),
            style: TextButton.styleFrom(
              foregroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.of(context).pop(true),
            child: Text(action),
          ),
        ],
      ),
    );
    // A dialog dismissed by tapping outside it returns null, and the answer to
    // "shall I delete this?" that nobody gave is no.
    return confirmed ?? false;
  }

  /// The app bar while cards are selected: how many, a way out, and what can
  /// be done to them.
  ///
  /// The actions follow the notes rather than the shelf, because a search can
  /// select from the grid and the archive at once. Thrown-away notes can only
  /// be brought back or deleted for good; anything else can be coloured,
  /// labelled, archived or thrown away.
  AppBar _selectionBar(List<NoteSummary> selected, List<NoteSummary> shown) {
    final count = selected.length;
    final trashed = selected.isNotEmpty && selected.every((n) => n.trashed);
    final archived = selected.isNotEmpty && selected.every((n) => n.archived);
    final pin = selected.any((note) => !note.pinned);
    final canSelectAll = !_searching && shown.length > count;
    return AppBar(
      leading: IconButton(
        key: const Key('selection-close'),
        icon: const Icon(Icons.close),
        tooltip: 'Clear selection',
        onPressed: _clearSelection,
      ),
      title: EasyEntrance(
        child: Text(
          '$count',
          key: const Key('selection-count'),
          semanticsLabel: '$count selected',
        ),
      ),
      actions: trashed
          ? [
              IconButton(
                key: const Key('bulk-restore'),
                icon: const Icon(Icons.restore_from_trash),
                tooltip: 'Restore',
                onPressed: () => _trashSelected(selected, false),
              ),
              IconButton(
                key: const Key('bulk-delete-forever'),
                icon: const Icon(Icons.delete_forever_outlined),
                tooltip: 'Delete permanently',
                onPressed: () => _forgetSelected(selected),
              ),
              if (canSelectAll)
                IconButton(
                  key: const Key('bulk-select-all'),
                  icon: const Icon(Icons.select_all),
                  tooltip: 'Select all',
                  onPressed: () => _selectAll(shown),
                ),
            ]
          : [
              IconButton(
                key: const Key('bulk-colour'),
                icon: const Icon(Icons.palette_outlined),
                tooltip: 'Colour',
                onPressed: () => _colourSelected(selected),
              ),
              IconButton(
                key: const Key('bulk-labels'),
                icon: const Icon(Icons.label_outline),
                tooltip: 'Labels',
                onPressed: () => _labelSelected(selected),
              ),
              IconButton(
                key: const Key('bulk-archive'),
                icon: Icon(
                  archived ? Icons.unarchive_outlined : Icons.archive_outlined,
                ),
                tooltip: archived ? 'Unarchive' : 'Archive',
                onPressed: () => _archiveSelected(selected, !archived),
              ),
              IconButton(
                key: const Key('bulk-trash'),
                icon: const Icon(Icons.delete_outline),
                tooltip: 'Move to trash',
                onPressed: () => _trashSelected(selected, true),
              ),
              PopupMenuButton<VoidCallback>(
                key: const Key('selection-more'),
                tooltip: 'More',
                onSelected: (action) => action(),
                itemBuilder: (context) => [
                  PopupMenuItem(
                    key: const Key('bulk-pin'),
                    value: () => _pinSelected(selected),
                    child: Text(pin ? 'Pin' : 'Unpin'),
                  ),
                  // A reminder is a time for one note; several notes at the
                  // same minute is not something anybody has asked for.
                  if (count == 1)
                    PopupMenuItem(
                      key: const Key('bulk-reminders'),
                      value: () => _remindersFor(selected.single),
                      child: const Text('Reminders'),
                    ),
                  if (canSelectAll)
                    PopupMenuItem(
                      key: const Key('bulk-select-all'),
                      value: () => _selectAll(shown),
                      child: const Text('Select all'),
                    ),
                ],
              ),
            ],
    );
  }

  /// One transfers stream for the whole list, for the covers to wait on.
  late final Stream<List<AttachmentTransfer>> _transfers = widget.library
      .watchTransfers();

  @override
  Widget build(BuildContext context) => CoverScope(
    read: widget.library.readCover,
    transfers: _transfers,
    child: _list(context),
  );

  Widget _list(BuildContext context) {
    return StreamBuilder<List<NoteSummary>>(
      stream: widget.library.watchNotes(),
      initialData: widget.library.notes,
      builder: (context, snapshot) {
        final all = snapshot.data ?? const <NoteSummary>[];
        final notes = all.where(_where.holds).toList();
        // Read here rather than in the drawer, so that a label appearing,
        // being renamed or being thrown away follows the same rebuild every
        // other change to the list does.
        final tags = widget.library.tags;
        // The label this person is looking at, gone. That now means thrown away
        // or merged into another by a rename — *not* merely emptied, because a
        // label outlives its last note and an empty shelf is a real place to
        // stand. Back to the grid, because a shelf that does not exist cannot
        // say anything about why it is empty.
        final tag = _where.tag;
        final standing = tag == null
            ? null
            : tags.where((held) => held.id == tag).firstOrNull;
        if (tag != null && standing == null) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) _show(const _Where.shelf(_Shelf.notes));
          });
        }
        // Renamed while being looked at: keep the shelf, change the heading.
        if (standing != null && standing.name != _where.tagName) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) _show(_Where.tag(standing.id, standing.name));
          });
        }
        // A selected note that has left the screen — archived from another
        // device, say — is let go of, so the bar never counts what is not
        // there to see.
        final selected = _selectedIn(all);
        if (selected.length != _selected.length) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (!mounted) return;
            final kept = {
              for (final note in _selectedIn(widget.library.notes)) note.id,
            };
            if (kept.length != _selected.length) {
              setState(() => _selected = kept);
            }
          });
        }
        return PopScope(
          canPop: !_searching && !_selecting,
          onPopInvokedWithResult: (didPop, result) {
            if (didPop) return;
            // Back lets go of a selection first, and closes a search after.
            if (_selecting) {
              _clearSelection();
            } else if (_searching) {
              _closeSearch();
            }
          },
          child: Scaffold(
            drawerEnableOpenDragGesture: !_searching && !_selecting,
            appBar: _selecting
                ? _selectionBar(selected, notes)
                : AppBar(
                    automaticallyImplyLeading: !_searching,
                    titleSpacing: _searching ? 8 : null,
                    title: EasyEntrance(
                      key: _searching
                          ? const ValueKey('search-input')
                          : ValueKey('search-launcher:${_where.title}'),
                      child: _searching
                          ? _searchInput(autofocus: true)
                          : _searchLauncher(),
                    ),
                    actions: [
                      if (!_searching) ...[
                        IconButton(
                          key: const Key('layout-toggle'),
                          icon: Icon(
                            _layout == NoteLayout.grid
                                ? Icons.view_agenda_outlined
                                : Icons.grid_view,
                          ),
                          tooltip: _layout == NoteLayout.grid
                              ? 'Show as a list'
                              : 'Show as a grid',
                          onPressed: _toggleLayout,
                        ),
                        // Only where there is a trash and something in it. An empty
                        // trash with a button to empty it invites the one gesture that
                        // cannot be undone, for no result.
                        if (_where.shelf == _Shelf.trash && notes.isNotEmpty)
                          IconButton(
                            key: const Key('empty-trash-button'),
                            icon: const Icon(Icons.delete_sweep_outlined),
                            tooltip: 'Empty the trash',
                            onPressed: () => _emptyTrash(notes.length),
                          ),
                      ],
                    ],
                  ),
            drawer: _Shelves(
              current: _where,
              counts: {
                for (final shelf in _Shelf.values)
                  shelf: all.where(shelf.holds).length,
              },
              tags: tags,
              onChosen: _show,
              onEditLabels: () => _manageLabels(),
              onSyncDevices: _openSyncDevices,
              profile: widget.profile,
              onAbout: _openAbout,
              // A tapped invite link is the way in; this is for one that
              // arrived somewhere it could not be tapped.
              onJoin: widget.library.canJoin ? _join : null,
              watchSync: widget.library.watchSync,
              initialSyncStatus: widget.library.syncStatus,
            ),
            // Only where a new note would land. One made while looking at the
            // trash would appear in a grid that is not on screen, and one made
            // while searching would appear in results it does not match.
            floatingActionButton:
                _where.takesNewNotes && !_searching && !_selecting
                ? Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      // The kinds of note are folded away until asked for, so
                      // that the screen is the notes and one button; the button
                      // that asks for them is the one that says what they make.
                      AnimatedSize(
                        duration: EasyMotion.fast,
                        alignment: Alignment.bottomCenter,
                        curve: EasyMotion.enter,
                        child: _addShortcutsExpanded
                            ? _NoteTypeMenu(
                                onCreateText: () {
                                  setState(() => _addShortcutsExpanded = false);
                                  _createNote();
                                },
                                onCreateList: () {
                                  setState(() => _addShortcutsExpanded = false);
                                  _createNote(kind: NoteKind.checklist);
                                },
                                onCreateAudio: _createAudioNote,
                              )
                            : const SizedBox.shrink(),
                      ),
                      _CreateNoteButton(
                        expanded: _addShortcutsExpanded,
                        onPressed: () => setState(
                          () => _addShortcutsExpanded = !_addShortcutsExpanded,
                        ),
                      ),
                    ],
                  )
                : null,
            // The banner above content on both the home and detail views: a
            // failed local write is serious enough to stay visible until noticed.
            body: Column(
              children: [
                StorageBanner(storage: widget.library),
                InviteBanners(
                  invites: widget.library,
                  titleOf: _titleOf,
                  onReview: _review,
                  onJoined: _joinedInBackground,
                ),
                Expanded(child: _body(all, notes)),
              ],
            ),
          ),
        );
      },
    );
  }

  /// A persistent, pill-shaped entry point to the same local search used by
  /// the expanded field. Keeping it in the app bar makes search easy to reach
  /// while keeping the drawer available outside search.
  Widget _searchLauncher() {
    final theme = Theme.of(context);
    return Semantics(
      button: true,
      label: _where.shelf == _Shelf.trash
          ? 'Search Trash'
          : 'Search notes and archive',
      child: Material(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(28),
        child: InkWell(
          key: const Key('search-button'),
          borderRadius: BorderRadius.circular(28),
          onTap: _openSearch,
          child: SizedBox(
            height: 48,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Row(
                children: [
                  Icon(Icons.search, color: theme.colorScheme.onSurfaceVariant),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Text(
                      _where.shelf == _Shelf.trash
                          ? 'Search Trash'
                          : 'Search notes and archive',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyLarge?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  /// The focused search field shares the launcher pill's shape and color.
  Widget _searchInput({required bool autofocus}) {
    final theme = Theme.of(context);
    return Material(
      color: theme.colorScheme.surfaceContainerHighest,
      borderRadius: BorderRadius.circular(28),
      child: Row(
        children: [
          IconButton(
            key: const Key('search-close'),
            icon: const Icon(Icons.arrow_back),
            tooltip: 'Back to Home',
            onPressed: _closeSearch,
          ),
          Expanded(
            child: TextField(
              key: const Key('search-field'),
              controller: _search,
              autofocus: autofocus,
              textInputAction: TextInputAction.search,
              textAlignVertical: TextAlignVertical.center,
              decoration: InputDecoration(
                hintText: _searchHint,
                hintMaxLines: 1,
                hintFadeDuration: Duration.zero,
                border: InputBorder.none,
                isDense: true,
              ),
              onChanged: (value) => setState(() => _query = value),
            ),
          ),
          IconButton(
            key: const Key('search-clear'),
            icon: const Icon(Icons.close),
            tooltip: 'Clear search and return to filters',
            onPressed: _resetSearch,
          ),
        ],
      ),
    );
  }

  Widget _filterBrowser(
    List<NoteSummary> notes,
    Set<String> reminders,
  ) => _SearchFilterBrowser(
    notes: notes,
    tags: widget.library.tags,
    reminderNoteIds: reminders,
    onPerson: (person) => _selectFilter(
      'Search with ${person.displayName}',
      (note, _) => note.collaborators.any((p) => p.id == person.id),
    ),
    onKind: (kind) => _selectFilter(
      kind == NoteKind.checklist ? 'Search in Checklists' : 'Search in Notes',
      (note, _) => note.kind == kind,
    ),
    // Whatever the note's kind: a checklist can hold a photo as well as a
    // text note can.
    onMedia: (kind) => _selectFilter(
      kind == AttachmentKind.image ? 'Search in Images' : 'Search in Audio',
      (note, _) =>
          kind == AttachmentKind.image ? note.images > 0 : note.audio > 0,
    ),
    onLabel: (id) => _selectFilter(
      'Search in ${widget.library.tags.where((tag) => tag.id == id).first.name}',
      (note, _) => note.tagIds.contains(id),
    ),
    onColour: (colour) => _selectFilter(
      colour == 'none'
          ? 'Search in notes without colour'
          : 'Search in ${_colourName(colour)} notes',
      (note, _) => (note.colour?.name ?? 'none') == colour,
    ),
    onReminder: (reminder) => _selectFilter(
      reminder == _ReminderFilter.hasReminder
          ? 'Search in notes with reminders'
          : 'Search in notes without reminders',
      (note, ids) =>
          ids.contains(note.id) == (reminder == _ReminderFilter.hasReminder),
    ),
  );

  /// The grid, the results or the empty line, whichever this screen is showing.
  Widget _body(List<NoteSummary> all, List<NoteSummary> notes) {
    final reminders = {
      for (final reminder in widget.library.reminders) reminder.noteId,
    };
    if (!_searching) {
      final collection = notes.isEmpty
          ? LayoutBuilder(
              builder: (context, constraints) => SingleChildScrollView(
                physics: const AlwaysScrollableScrollPhysics(),
                child: SizedBox(
                  height: constraints.maxHeight,
                  child: Center(
                    key: const Key('no-notes'),
                    child: Text(_where.empty),
                  ),
                ),
              ),
            )
          : _NoteCollection(
              notes: notes,
              reminderNoteIds: reminders,
              layout: _layout,
              cardKey: _cardKeyFor,
              expanded: _expandedNoteId,
              selected: _selected,
              onTap: _tap,
              onHold: _hold,
              // Nothing is carried while cards are selected: a long press
              // there adds a card to the selection instead.
              onMove: _selecting ? null : _moveNote,
            );
      if (_where == const _Where.shelf(_Shelf.notes) &&
          widget.profile.url != null) {
        return RefreshIndicator(
          onRefresh: widget.library.syncNow,
          child: collection,
        );
      }
      return collection;
    }

    final query = _query.trim();
    final scopedNotes = all.where(_inSearchScope).toList();
    return Column(
      children: [
        Expanded(
          child: _showFilters
              ? _filterBrowser(scopedNotes, reminders)
              : query.isNotEmpty
              ? _SearchResults(
                  library: widget.library,
                  query: query,
                  notes: all,
                  reminderNoteIds: reminders,
                  layout: _layout,
                  cardKey: _cardKeyFor,
                  expanded: _expandedNoteId,
                  matches: (note) =>
                      _inSearchScope(note) && _matchesFilter(note, reminders),
                  selected: _selected,
                  onTap: _tap,
                  onHold: _hold,
                )
              : _FilteredNotes(
                  notes: [
                    for (final note in scopedNotes)
                      if (_matchesFilter(note, reminders)) note,
                  ],
                  reminderNoteIds: reminders,
                  layout: _layout,
                  cardKey: _cardKeyFor,
                  expanded: _expandedNoteId,
                  selected: _selected,
                  onTap: _tap,
                  onHold: _hold,
                ),
        ),
      ],
    );
  }

  /// A tap opens a note, or — while cards are selected — adds it to the
  /// selection or takes it away.
  void _tap(String id, Rect? origin) =>
      _selecting ? _toggle(id) : _openNote(id, origin: origin);

  /// A card was dropped on another. The library is told which two notes they
  /// were and works out the order key; nothing here counts positions, because
  /// what is on screen is a shelf or a label and the order is over the lot.
  void _moveNote(String id, String targetId) {
    widget.library.moveNoteTo(id, targetId);
  }
}

/// How long a tap waits for the core before the note screen goes up anyway.
///
/// Two frames at 60 Hz: long enough for a note the core already has, short
/// enough that the tap still feels like it did something. The press ripple and
/// the shrunken card are on screen for all of it.
const _kOpenWait = Duration(milliseconds: 32);

/// The note screen, from the first frame of the tap rather than from when the
/// core answers.
///
/// Opening a note is a round trip. When it answers inside [_kOpenWait] the list
/// hands the core over and this is the real screen from the first frame; when
/// it does not, the expand route's surface is already the note's colour, so this
/// fills it with the same chrome — an app bar with a way back — and swaps in the
/// real screen when the note is open.
class _OpeningNote extends StatelessWidget {
  const _OpeningNote({
    required this.opening,
    required this.colour,
    required this.builder,
    this.ready,
  });

  final Future<NoteCore> opening;

  /// The core, when it answered before the trip started. Then there is nothing
  /// to wait for and nothing to subscribe to.
  final NoteCore? ready;
  final Color colour;
  final Widget Function(NoteCore core) builder;

  @override
  Widget build(BuildContext context) {
    final answered = ready;
    if (answered != null) return builder(answered);
    return FutureBuilder<NoteCore>(
      future: opening,
      builder: (context, snapshot) {
        if (snapshot.hasError) {
          return Scaffold(
            backgroundColor: colour,
            appBar: _chrome(),
            body: const Center(child: Text('Could not open this note.')),
          );
        }
        final core = snapshot.data;
        if (core == null) {
          return Scaffold(
            key: const Key('note-opening'),
            backgroundColor: colour,
            appBar: _chrome(),
          );
        }
        return builder(core);
      },
    );
  }

  /// The bar the real screen puts up, down to its share action: the header
  /// should not grow a button halfway through the trip.
  AppBar _chrome() => AppBar(
    backgroundColor: colour,
    actions: const [
      IconButton(icon: Icon(Icons.person_add_outlined), onPressed: null),
    ],
  );
}

/// Search browse view: choose one filter, then optionally search within it.
/// People use the verified directory cached with each note summary.
class _SearchFilterBrowser extends StatefulWidget {
  const _SearchFilterBrowser({
    required this.notes,
    required this.tags,
    required this.reminderNoteIds,
    required this.onKind,
    required this.onMedia,
    required this.onLabel,
    required this.onColour,
    required this.onReminder,
    required this.onPerson,
  });

  final List<NoteSummary> notes;
  final List<Tag> tags;
  final Set<String> reminderNoteIds;
  final ValueChanged<NoteKind> onKind;
  final ValueChanged<AttachmentKind> onMedia;
  final ValueChanged<String> onLabel;
  final ValueChanged<String> onColour;
  final ValueChanged<_ReminderFilter> onReminder;
  final ValueChanged<Collaborator> onPerson;

  @override
  State<_SearchFilterBrowser> createState() => _SearchFilterBrowserState();
}

class _SearchFilterBrowserState extends State<_SearchFilterBrowser> {
  bool _showAllLabels = false;
  bool _showAllColours = false;

  int _kindCount(NoteKind kind) =>
      widget.notes.where((note) => note.kind == kind).length;

  int _labelCount(Tag tag) =>
      widget.notes.where((note) => note.tagIds.contains(tag.id)).length;

  int _colourCount(String value) => widget.notes
      .where((note) => (note.colour?.name ?? 'none') == value)
      .length;

  int _reminderCount(_ReminderFilter filter) => widget.notes.where((note) {
    final has = widget.reminderNoteIds.contains(note.id);
    return filter == _ReminderFilter.hasReminder ? has : !has;
  }).length;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final labels = widget.tags
        .where((tag) => _labelCount(tag) > 0)
        .toList(growable: false);
    final people =
        <String, Collaborator>{
          for (final note in widget.notes)
            for (final person in note.collaborators) person.id: person,
        }.values.toList()..sort(
          (a, b) => a.displayName.toLowerCase().compareTo(
            b.displayName.toLowerCase(),
          ),
        );
    final visibleLabels = _showAllLabels ? labels : labels.take(8).toList();
    final colourValues = [
      'none',
      for (final colour in NoteColour.values) colour.name,
    ];
    final visibleColours = _showAllColours
        ? colourValues
        : colourValues.take(5).toList();

    return ListView(
      key: const Key('search-filter-browser'),
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 20),
      children: [
        _FilterSection(
          title: 'Types',
          children: [
            _FacetChip(
              key: const Key('filter-type-text'),
              icon: Icons.notes_outlined,
              label: 'Notes',
              count: _kindCount(NoteKind.text),
              onSelected: () => widget.onKind(NoteKind.text),
            ),
            _FacetChip(
              key: const Key('filter-type-checklist'),
              icon: Icons.check_box_outlined,
              label: 'Checklists',
              count: _kindCount(NoteKind.checklist),
              onSelected: () => widget.onKind(NoteKind.checklist),
            ),
            _FacetChip(
              key: const Key('filter-type-images'),
              icon: Icons.image_outlined,
              label: 'Images',
              count: widget.notes.where((note) => note.images > 0).length,
              onSelected: () => widget.onMedia(AttachmentKind.image),
            ),
            _FacetChip(
              key: const Key('filter-type-audio'),
              icon: Icons.audiotrack_outlined,
              label: 'Audio',
              count: widget.notes.where((note) => note.audio > 0).length,
              onSelected: () => widget.onMedia(AttachmentKind.audio),
            ),
          ],
        ),
        _FilterSection(
          title: 'Labels',
          trailing: labels.length > 8
              ? TextButton(
                  onPressed: () =>
                      setState(() => _showAllLabels = !_showAllLabels),
                  child: Text(_showAllLabels ? 'Less' : 'More'),
                )
              : null,
          children: [
            if (visibleLabels.isEmpty)
              Text(
                'Labels appear here when notes have them.',
                style: theme.textTheme.bodySmall,
              )
            else
              for (final tag in visibleLabels)
                _FacetChip(
                  key: Key('filter-label-${tag.id}'),
                  icon: Icons.label_outline,
                  label: tag.name,
                  count: _labelCount(tag),
                  onSelected: () => widget.onLabel(tag.id),
                ),
          ],
        ),
        _FilterSection(
          title: 'Reminders',
          children: [
            _FacetChip(
              key: const Key('filter-reminders-has'),
              icon: Icons.notifications_outlined,
              label: 'Has reminder',
              count: _reminderCount(_ReminderFilter.hasReminder),
              onSelected: () => widget.onReminder(_ReminderFilter.hasReminder),
            ),
            _FacetChip(
              key: const Key('filter-reminders-none'),
              icon: Icons.notifications_off_outlined,
              label: 'No reminder',
              count: _reminderCount(_ReminderFilter.noReminder),
              onSelected: () => widget.onReminder(_ReminderFilter.noReminder),
            ),
          ],
        ),
        _FilterSection(
          title: 'People',
          children: [
            if (people.isEmpty)
              Text(
                'Collaborators appear here after an invite is approved.',
                style: theme.textTheme.bodySmall,
              )
            else
              for (final person in people)
                ActionChip(
                  key: Key('filter-person-${person.id}'),
                  avatar: CircleAvatar(
                    child: Text(
                      person.initials,
                      style: const TextStyle(fontSize: 11),
                    ),
                  ),
                  label: Text(
                    '${person.displayName}  ${widget.notes.where((n) => n.collaborators.any((p) => p.id == person.id)).length}',
                  ),
                  tooltip: 'Device ${person.id}',
                  onPressed: () => widget.onPerson(person),
                ),
          ],
        ),
        _FilterSection(
          title: 'Colours',
          trailing: colourValues.length > 5
              ? TextButton(
                  onPressed: () =>
                      setState(() => _showAllColours = !_showAllColours),
                  child: Text(_showAllColours ? 'Less' : 'More'),
                )
              : null,
          children: [
            for (final value in visibleColours)
              _ColourFacet(
                key: Key('filter-color-$value'),
                value: value,
                count: _colourCount(value),
                onSelected: () => widget.onColour(value),
              ),
          ],
        ),
      ],
    );
  }
}

class _FilterSection extends StatelessWidget {
  const _FilterSection({
    required this.title,
    required this.children,
    this.trailing,
  });

  final String title;
  final List<Widget> children;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 14),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(title, style: Theme.of(context).textTheme.titleSmall),
            ),
            ?trailing,
          ],
        ),
        const SizedBox(height: 6),
        Wrap(spacing: 8, runSpacing: 0, children: children),
      ],
    ),
  );
}

class _FacetChip extends StatelessWidget {
  const _FacetChip({
    super.key,
    required this.icon,
    required this.label,
    required this.count,
    required this.onSelected,
  });

  final IconData icon;
  final String label;
  final int count;
  final VoidCallback onSelected;

  @override
  Widget build(BuildContext context) => ActionChip(
    avatar: Icon(icon, size: 18),
    label: Text('$label  $count'),
    onPressed: onSelected,
  );
}

class _ColourFacet extends StatelessWidget {
  const _ColourFacet({
    super.key,
    required this.value,
    required this.count,
    required this.onSelected,
  });

  final String value;
  final int count;
  final VoidCallback onSelected;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colour = value == 'none'
        ? theme.colorScheme.surface
        : noteColourTint(context, NoteColour.values.byName(value))!;
    return Tooltip(
      message: '${_colourName(value)}, $count ${count == 1 ? 'note' : 'notes'}',
      child: InkWell(
        onTap: onSelected,
        customBorder: const StadiumBorder(),
        child: Container(
          width: 68,
          height: 42,
          margin: const EdgeInsets.only(bottom: 8),
          decoration: BoxDecoration(
            color: colour,
            borderRadius: BorderRadius.circular(24),
            border: Border.all(color: theme.dividerColor, width: 1),
          ),
          child: Center(
            child: value == 'none'
                ? Icon(Icons.format_color_reset_outlined, size: 18)
                : Text('$count', style: theme.textTheme.labelMedium),
          ),
        ),
      ),
    );
  }
}

class _FilteredNotes extends StatelessWidget {
  const _FilteredNotes({
    required this.notes,
    required this.reminderNoteIds,
    required this.layout,
    required this.cardKey,
    required this.expanded,
    required this.selected,
    required this.onTap,
    required this.onHold,
  });

  final List<NoteSummary> notes;
  final Set<String> reminderNoteIds;
  final NoteLayout layout;
  final GlobalKey Function(String id) cardKey;

  /// The note whose screen is open, whose card must not draw behind it.
  final ValueListenable<String?> expanded;
  final Set<String> selected;
  final void Function(String id, Rect? origin) onTap;
  final void Function(NoteSummary note) onHold;

  @override
  Widget build(BuildContext context) => notes.isEmpty
      ? const Center(
          key: Key('no-filter-matches'),
          child: Text('No notes match these filters.'),
        )
      : _NoteCollection(
          notes: notes,
          reminderNoteIds: reminderNoteIds,
          layout: layout,
          cardKey: cardKey,
          expanded: expanded,
          selected: selected,
          onTap: onTap,
          onHold: onHold,
        );
}

/// What the library found, drawn as the same cards with the matching line on
/// them.
///
/// A widget of its own because searching is the one thing on this screen that
/// is not a function of the note list: the text is inside the notes and only
/// the library can read it, so this asks and waits. It asks again whenever the
/// query changes *or* the list does, which is what makes a result follow an edit
/// arriving from another device while the results are on screen.
class _SearchResults extends StatefulWidget {
  const _SearchResults({
    required this.library,
    required this.query,
    required this.notes,
    required this.reminderNoteIds,
    required this.layout,
    required this.cardKey,
    required this.expanded,
    required this.matches,
    required this.selected,
    required this.onTap,
    required this.onHold,
  });

  final NoteLibrary library;
  final String query;
  final List<NoteSummary> notes;
  final Set<String> reminderNoteIds;
  final NoteLayout layout;
  final GlobalKey Function(String id) cardKey;

  /// The note whose screen is open, whose card must not draw behind it.
  final ValueListenable<String?> expanded;
  final bool Function(NoteSummary) matches;
  final Set<String> selected;
  final void Function(String id, Rect? origin) onTap;
  final void Function(NoteSummary note) onHold;

  @override
  State<_SearchResults> createState() => _SearchResultsState();
}

class _SearchResultsState extends State<_SearchResults> {
  List<SearchHit>? _hits;

  /// Which search is the current one. Typing is faster than a search returns,
  /// and without this the answer to "aspar" can arrive after the answer to
  /// "asparagus" and be shown instead of it.
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _run();
  }

  @override
  void didUpdateWidget(_SearchResults old) {
    super.didUpdateWidget(old);
    if (old.query != widget.query || !listEquals(old.notes, widget.notes)) {
      _run();
    }
  }

  Future<void> _run() async {
    final generation = ++_generation;
    final hits = await widget.library.search(widget.query);
    if (!mounted || generation != _generation) return;
    setState(() => _hits = hits);
  }

  @override
  Widget build(BuildContext context) {
    final hits = _hits;
    // The first frame, before the first answer. Blank rather than a spinner: a
    // local search over a household's lists is done well inside a frame, and a
    // spinner that flashes says something untrue about what is happening.
    if (hits == null) return const SizedBox.shrink();

    final found = hits.where((hit) => widget.matches(hit.note)).toList();
    if (found.isEmpty) {
      return Center(
        key: const Key('no-matches'),
        child: Text(
          widget.query.isEmpty
              ? 'No notes match these filters.'
              : 'Nothing here matches "${widget.query}".',
        ),
      );
    }
    return _NoteCollection(
      notes: [for (final hit in found) hit.note],
      reminderNoteIds: widget.reminderNoteIds,
      matched: {for (final hit in found) hit.note.id: hit.matched},
      layout: widget.layout,
      cardKey: widget.cardKey,
      expanded: widget.expanded,
      selected: widget.selected,
      onTap: widget.onTap,
      onHold: widget.onHold,
    );
  }
}

/// The grid, or the single column. Both draw the same cards.
class _NoteCollection extends StatefulWidget {
  const _NoteCollection({
    required this.notes,
    required this.reminderNoteIds,
    required this.layout,
    required this.cardKey,
    required this.expanded,
    required this.onTap,
    required this.onHold,
    this.selected = const {},
    this.onMove,
    this.matched = const {},
  });

  /// The notes picked by a long press, whose cards are drawn as picked.
  final Set<String> selected;

  final List<NoteSummary> notes;
  final Set<String> reminderNoteIds;
  final NoteLayout layout;

  /// Where each card's render object lives, owned by the list screen so that
  /// the expand route can measure the card it is growing out of — and, on the
  /// way back, the card as it is by then.
  final GlobalKey Function(String id) cardKey;

  /// The note whose screen is open. Its card keeps its place but draws
  /// nothing, because the screen above it *is* that card.
  final ValueListenable<String?> expanded;

  /// A tap, with where the card was at the time: the expand route grows from
  /// that rectangle rather than from the note's place in an abstract list.
  final void Function(String id, Rect? origin) onTap;
  final void Function(NoteSummary note) onHold;

  /// Where a card was dropped: the note that moved, and the note it landed on.
  ///
  /// Null on the one collection that cannot be reordered — search results, where
  /// the cards on screen are the answer to a question rather than the grid, and
  /// dropping one on another would move two notes past each other in an order
  /// most of which is not being shown.
  final void Function(String id, String targetId)? onMove;

  /// The line a search matched, per note. Empty when this is the plain list.
  final Map<String, String> matched;

  @override
  State<_NoteCollection> createState() => _NoteCollectionState();
}

class _NoteCollectionState extends State<_NoteCollection>
    with SingleTickerProviderStateMixin {
  String? _draggedId;
  String? _targetId;
  List<NoteSummary>? _dragStartNotes;

  /// How tall the carried card was when it was picked up, so the gap it leaves
  /// is exactly the size of the card that left it.
  double? _dragHeight;
  bool _awaitingDrop = false;
  List<String>? _expectedDropIds;
  int _animationGeneration = 0;
  Map<String, Offset> _offsets = {};
  late final AnimationController _positionAnimation = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 220),
  );

  @override
  void dispose() {
    _positionAnimation.dispose();
    super.dispose();
  }

  GlobalKey _keyFor(String id) => widget.cardKey(id);

  Offset? _positionOf(String id) {
    final object = _keyFor(id).currentContext?.findRenderObject();
    if (object is! RenderBox || !object.attached) return null;
    return object.localToGlobal(Offset.zero);
  }

  /// The card's rectangle on screen, for the route that grows out of it.
  Rect? _rectOf(String id) {
    final object = _keyFor(id).currentContext?.findRenderObject();
    if (object is! RenderBox || !object.attached) return null;
    return object.localToGlobal(Offset.zero) & object.size;
  }

  Offset _visibleOffset(String id) =>
      (_offsets[id] ?? Offset.zero) * (1 - _positionAnimation.value);

  /// Apply [change] to the layout, preserving each card's current screen
  /// position across it and then animating its inverse offset back to zero
  /// (the FLIP layout animation).
  void _animateLayoutChange(VoidCallback change) {
    final generation = ++_animationGeneration;
    _positionAnimation.stop();
    final oldPositions = {
      for (final note in widget.notes) note.id: ?_positionOf(note.id),
    };
    final oldOffsets = {
      for (final note in widget.notes) note.id: _visibleOffset(note.id),
    };
    setState(change);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || generation != _animationGeneration) return;
      final offsets = <String, Offset>{};
      for (final entry in oldPositions.entries) {
        final current = _positionOf(entry.key);
        if (current == null) continue;
        // The current render position still includes the previous transform.
        final layoutPosition = current - (oldOffsets[entry.key] ?? Offset.zero);
        final offset = entry.value - layoutPosition;
        if (offset.distance > 0.5) offsets[entry.key] = offset;
      }
      setState(() => _offsets = offsets);
      _positionAnimation.forward(from: 0);
    });
  }

  void _animateToTarget(String? id) {
    if (_targetId == id) return;
    _animateLayoutChange(() => _targetId = id);
  }

  void _setTarget(String? id) {
    // The drop has already happened; the preview belongs to the pointer that
    // let go, and the library's answer is on its way.
    if (_awaitingDrop) return;
    // A target is only replaced, never cleared on the way out of a card: the
    // gap can end up under the finger, and a preview that went back to the
    // start would move it out again on the next pointer event.
    _animateToTarget(id);
  }

  void _startDrag(String id) {
    final height = _rectOf(id)?.height;
    setState(() {
      _draggedId = id;
      _targetId = null;
      _dragStartNotes = List.of(widget.notes);
      _dragHeight = height;
      _awaitingDrop = false;
      _expectedDropIds = null;
    });
  }

  void _finishDrag(bool accepted) {
    if (_draggedId == null && _targetId == null) return;
    if (accepted && _targetId != null) {
      _awaitingDrop = true;
      _expectedDropIds = _previewOrder(
        _dragStartNotes ?? widget.notes,
      ).map((note) => note.id).toList();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _settleAcceptedDrop();
      });
      return;
    }
    _clearDrag();
  }

  /// Let the grid go back to the order it is actually in, animating the cards
  /// from where the preview left them.
  void _clearDrag() {
    if (_draggedId == null && _targetId == null) return;
    _animateLayoutChange(() {
      _draggedId = null;
      _targetId = null;
      _dragStartNotes = null;
      _dragHeight = null;
      _awaitingDrop = false;
      _expectedDropIds = null;
    });
  }

  @override
  void didUpdateWidget(_NoteCollection oldWidget) {
    super.didUpdateWidget(oldWidget);
    // Reordering was switched off under a drag that had not been dropped —
    // a selection started — so there is no drop to wait for.
    if (widget.onMove == null && oldWidget.onMove != null) {
      _clearDrag();
      return;
    }
    if (_awaitingDrop) _settleAcceptedDrop();
  }

  void _settleAcceptedDrop() {
    if (!_awaitingDrop) return;
    final actual = widget.notes.map((note) => note.id).toList();
    // Either the list now says what the preview predicted, or it has changed
    // to something else. Waiting only for the first answer would leave the
    // preview on screen for good if the library resolved the move another way.
    final start = _dragStartNotes?.map((note) => note.id).toList();
    final settled =
        listEquals(_expectedDropIds, actual) || !listEquals(start, actual);
    if (!settled) return;
    _animateLayoutChange(() {
      _awaitingDrop = false;
      _expectedDropIds = null;
      _draggedId = null;
      _targetId = null;
      _dragStartNotes = null;
      _dragHeight = null;
    });
  }

  List<NoteSummary> _previewOrder(List<NoteSummary> notes) {
    final draggedId = _draggedId;
    final targetId = _targetId;
    if (draggedId == null || targetId == null) return notes;
    final from = notes.indexWhere((note) => note.id == draggedId);
    final target = notes.indexWhere((note) => note.id == targetId);
    if (from < 0 || target < 0 || from == target) return notes;

    final reordered = [...notes];
    final dragged = reordered.removeAt(from);
    // A note takes the target's place: moving down lands after it, and moving
    // up lands before it, as implemented by move_note_to in the core.
    final targetAfterRemoval = reordered.indexWhere(
      (note) => note.id == targetId,
    );
    final insertion = from < target
        ? targetAfterRemoval + 1
        : targetAfterRemoval;
    reordered.insert(insertion, dragged);
    return reordered;
  }

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<String?>(
      valueListenable: widget.expanded,
      builder: (context, expandedId, _) => _build(context, expandedId),
    );
  }

  Widget _build(BuildContext context, String? expandedId) {
    final notes = _previewOrder(widget.notes);
    final move = widget.onMove;
    final cards = [
      for (final note in notes)
        _NoteCard(
          key: ValueKey(note.id),
          note: note,
          hasReminder: widget.reminderNoteIds.contains(note.id),
          matched: widget.matched[note.id],
          hidden: note.id == expandedId,
          selected: widget.selected.contains(note.id),
          onTap: () => widget.onTap(note.id, _rectOf(note.id)),
          // Held by the wrapper where there is one, because a long press there
          // is the start of a drag and only turns into this when nothing moved.
          onHold: move == null ? () => widget.onHold(note) : null,
        ),
    ];
    Widget wrap(_NoteCard card) => move == null
        ? card
        : _DraggableCard(
            card: card,
            dragging: card.note.id == _draggedId,
            placeholderHeight: _dragHeight,
            pendingTargetId: _targetId,
            onMove: move,
            onHold: () => widget.onHold(card.note),
            onTargetChanged: _setTarget,
            onDragStarted: () => _startDrag(card.note.id),
            onDragFinished: _finishDrag,
          );
    Widget position(_NoteCard card) => AnimatedBuilder(
      key: ValueKey(card.note.id),
      animation: _positionAnimation,
      builder: (context, child) => Transform.translate(
        offset: _visibleOffset(card.note.id),
        // While a card is being carried, a target is worked out from where
        // the cards will settle, not from where they are sliding to. Following
        // the animation makes the card under the finger change on its own,
        // and the preview changes with it.
        transformHitTests: _draggedId == null,
        child: child,
      ),
      child: KeyedSubtree(key: _keyFor(card.note.id), child: wrap(card)),
    );
    return widget.layout == NoteLayout.grid
        ? _MasonryGrid(
            wrap: position,
            draggedId: _draggedId,
            targetId: _targetId,
            dragOrder: _dragStartNotes,
            children: cards,
          )
        : ListView(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
            children: [
              for (final card in cards)
                Padding(
                  padding: const EdgeInsets.only(bottom: 8),
                  child: position(card),
                ),
            ],
          );
  }
}

/// A card that can be picked up, and that a picked-up card can be dropped on.
///
/// The gesture is Keep's, and it has to do two jobs with one press: holding a
/// card lifts it, and letting go without having moved it selects the card.
/// They cannot be two gestures — a long press that both starts a drag and
/// selects is one pointer in two places — so the drag is the gesture and the
/// selection is what a drag that went nowhere means.
///
/// A drop is refused across the pin line. Pinned notes are drawn first but share
/// one order with the rest ([ADR 0008](../../../docs/adr/0008-account-document.md)),
/// so dragging a pinned card among the unpinned ones would write a move that
/// nothing on screen could show — the card would spring back to the top and look
/// broken. Unpin it and it lands where it was dropped.
///
/// A carried card is not drawn again in place: its brick is a gap the size of
/// the card, which is also where it would land. The gap accepts the carried
/// card, because letting go there is letting go on the preview's target; it
/// never accepts any other card.
class _DraggableCard extends StatefulWidget {
  const _DraggableCard({
    required this.card,
    required this.onMove,
    required this.onHold,
    required this.onTargetChanged,
    required this.onDragStarted,
    required this.onDragFinished,
    this.dragging = false,
    this.placeholderHeight,
    this.pendingTargetId,
  });

  final _NoteCard card;
  final void Function(String id, String targetId) onMove;
  final VoidCallback onHold;
  final ValueChanged<String?> onTargetChanged;
  final VoidCallback onDragStarted;
  final ValueChanged<bool> onDragFinished;

  /// Whether this is the card under the finger. It draws the gap it left
  /// rather than itself.
  final bool dragging;

  /// The card's height when it was picked up, so the gap it leaves is the
  /// size of the card that left it and the grid does not jump as it appears.
  final double? placeholderHeight;

  /// Where a drop would land right now, while this card is the one being
  /// carried. Letting go over the gap is letting go over this card's own
  /// target: the gap is drawn where the drop will land.
  final String? pendingTargetId;

  @override
  State<_DraggableCard> createState() => _DraggableCardState();
}

class _DraggableCardState extends State<_DraggableCard> {
  /// How far the pointer has travelled since the card was picked up, and so
  /// whether this is a drag or a hand resting on a screen.
  ///
  /// The path's length rather than the distance from where it started: a card
  /// dragged out and back is still a card that was dragged, and a first update
  /// that already carries the whole movement — which is every drag a test makes
  /// — has no earlier position to be measured against.
  double _travelled = 0;
  bool _moved = false;

  NoteSummary get _note => widget.card.note;

  void _started() {
    _travelled = 0;
    _moved = false;
    widget.onDragStarted();
  }

  void _updated(DragUpdateDetails details) {
    _travelled += details.delta.distance;
    if (_travelled > kTouchSlop) _moved = true;
  }

  void _ended(DraggableDetails details) {
    widget.onDragFinished(details.wasAccepted);
    // Dropped on nothing, having gone nowhere: the press was a press. A drag
    // that travelled and was refused — across the pin line, or onto empty
    // space — is a drag that missed, and selecting the card under a finger
    // that was aiming somewhere else would be a surprise.
    if (!details.wasAccepted && !_moved) widget.onHold();
  }

  /// Whether a drop of [dragged] here would mean something: not on itself, and
  /// not across the pin line.
  bool _accepts(NoteSummary dragged) =>
      dragged.id != _note.id && dragged.pinned == _note.pinned;

  @override
  Widget build(BuildContext context) {
    final pending = widget.pendingTargetId;
    return DragTarget<NoteSummary>(
      onWillAcceptWithDetails: (details) => widget.dragging
          ? pending != null && details.data.id == _note.id
          : _accepts(details.data),
      onAcceptWithDetails: (details) {
        final target = widget.dragging ? pending : _note.id;
        if (target != null) widget.onMove(details.data.id, target);
      },
      onMove: (details) {
        // Rejected targets are told about the pointer too. Only a card that
        // would take the drop may move the preview; otherwise crossing the
        // pin line, or moving over the gap, would reorder the grid for a drop
        // that cannot happen. Over the gap the target is already decided.
        if (!widget.dragging && _accepts(details.data)) {
          widget.onTargetChanged(_note.id);
        }
      },
      builder: (context, _, _) => LayoutBuilder(
        builder: (context, constraints) => LongPressDraggable<NoteSummary>(
          data: _note,
          onDragStarted: _started,
          onDragUpdate: _updated,
          onDragEnd: _ended,
          feedback: SizedBox(
            width: constraints.maxWidth,
            child: Opacity(
              opacity: 0.9,
              child: Material(
                type: MaterialType.transparency,
                child: widget.card,
              ),
            ),
          ),
          // The gap the card came out of, exactly as tall as the card was, so
          // the columns close up around a hole rather than around a faded copy
          // of the card that is under the finger. It is drawn where the card
          // will land, so letting go on it commits the target already chosen.
          childWhenDragging: SizedBox(
            height: widget.placeholderHeight ?? 0,
            child: widget.placeholderHeight == null
                ? null
                : const _DropPlaceholder(),
          ),
          child: widget.card,
        ),
      ),
    );
  }
}

/// The hole a carried card leaves behind.
///
/// It is drawn where the card will land, so letting go over it commits the
/// drop already chosen. It never chooses a different target: a card cannot be
/// dropped onto itself.
class _DropPlaceholder extends StatelessWidget {
  const _DropPlaceholder();

  @override
  Widget build(BuildContext context) {
    return ExcludeSemantics(
      child: Card(
        key: const Key('drag-placeholder'),
        margin: EdgeInsets.zero,
        color: Theme.of(
          context,
        ).colorScheme.surfaceContainerHighest.withValues(alpha: 0.55),
      ),
    );
  }
}

/// The three shelves and this person's labels, with how many notes are on each.
class _Shelves extends StatelessWidget {
  const _Shelves({
    required this.current,
    required this.counts,
    required this.tags,
    required this.onChosen,
    required this.onEditLabels,
    required this.onSyncDevices,
    required this.profile,
    required this.onAbout,
    required this.onJoin,
    required this.watchSync,
    required this.initialSyncStatus,
  });

  final _Where current;
  final Map<_Shelf, int> counts;

  /// Every label this person has, in the order the library gives them. Empty
  /// until they make one, and *not* empty again when they take the last one
  /// off: a label outlives the notes that carried it, so a shelf here can
  /// legitimately say `0`.
  final List<Tag> tags;
  final void Function(_Where) onChosen;
  final VoidCallback onEditLabels;

  final VoidCallback onSyncDevices;
  final RelayProfile profile;
  final VoidCallback onAbout;
  final VoidCallback? onJoin;
  final Stream<SyncStatus> Function() watchSync;
  final SyncStatus initialSyncStatus;

  static const _icons = {
    _Shelf.notes: Icons.lightbulb_outline,
    _Shelf.archive: Icons.archive_outlined,
    _Shelf.trash: Icons.delete_outline,
  };

  @override
  Widget build(BuildContext context) {
    void go(_Where where) {
      Navigator.of(context).pop();
      onChosen(where);
    }

    return Drawer(
      child: ListView(
        padding: EdgeInsets.zero,
        children: [
          SafeArea(
            bottom: false,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 24, 16, 16),
              child: Text(
                'EasyNotes',
                style: Theme.of(context).textTheme.headlineSmall,
              ),
            ),
          ),
          for (final shelf in _Shelf.values)
            ListTile(
              key: Key('shelf-${shelf.name}'),
              leading: Icon(_icons[shelf]),
              title: Text(shelf == _Shelf.notes ? 'Notes' : shelf.title),
              trailing: Text('${counts[shelf] ?? 0}'),
              selected: current.tag == null && shelf == current.shelf,
              onTap: () => go(_Where.shelf(shelf)),
            ),
          const Divider(),
          Padding(
            padding: const EdgeInsets.only(left: 16, right: 8),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text('Labels', style: Theme.of(context).textTheme.labelMedium),
                TextButton(
                  key: const Key('edit-labels'),
                  onPressed: () {
                    Navigator.of(context).pop();
                    onEditLabels();
                  },
                  child: const Text('Edit'),
                ),
              ],
            ),
          ),
          if (tags.isEmpty)
            Padding(
              key: const Key('no-labels'),
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
              child: Text(
                'No labels yet',
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                  color: Theme.of(context).colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          for (final tag in tags)
            ListTile(
              key: Key('label-shelf-${tag.name}'),
              leading: const Icon(Icons.label_outline),
              title: Text(tag.name),
              trailing: Text('${tag.notes}'),
              selected: current.tag == tag.id,
              onTap: () => go(_Where.tag(tag.id, tag.name)),
            ),
          const Divider(),
          SyncStatusTile(
            watch: watchSync,
            initial: initialSyncStatus,
            hasRelay: profile.url != null,
            onTap: () {
              Navigator.of(context).pop();
              onSyncDevices();
            },
          ),
          if (onJoin != null)
            ListTile(
              key: const Key('join-entry'),
              leading: const Icon(Icons.group_add_outlined),
              title: const Text('Join with a link'),
              onTap: () {
                Navigator.of(context).pop();
                onJoin!();
              },
            ),
          const Divider(),
          ListTile(
            key: const Key('about-entry'),
            leading: const Icon(Icons.info_outline),
            title: const Text('About and privacy'),
            onTap: () {
              Navigator.of(context).pop();
              onAbout();
            },
          ),
        ],
      ),
    );
  }
}

/// A choice from a sheet that can itself be "nothing": no colour is a colour
/// to choose, and must not read as the sheet being dismissed.
@immutable
class _Chosen<T> {
  const _Chosen(this.value);
  final T value;
}

/// Every colour a note can be, and none, in a row to pick from.
class _ColourRow extends StatelessWidget {
  const _ColourRow({
    required this.chosen,
    required this.anyChosen,
    required this.onChosen,
  });

  /// The colour to mark as current, if [anyChosen]. Null there is "no
  /// colour", which is a choice too.
  final NoteColour? chosen;

  /// False when the notes do not share a colour, so nothing is marked.
  final bool anyChosen;
  final ValueChanged<NoteColour?> onChosen;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: 64,
    child: ListView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      children: [
        for (final colour in <NoteColour?>[null, ...NoteColour.values])
          _Swatch(
            colour: colour,
            chosen: anyChosen && chosen == colour,
            onTap: () => onChosen(colour),
          ),
      ],
    ),
  );
}

/// The small editor behind the reminders action. It deliberately uses the platform
/// date and time pickers: reminder entry is a calendar operation, and text
/// fields make an invalid date easier to enter than a valid one.
class _RemindersDialog extends StatefulWidget {
  const _RemindersDialog({required this.library, required this.note});

  final NoteLibrary library;
  final NoteSummary note;

  @override
  State<_RemindersDialog> createState() => _RemindersDialogState();
}

class _RemindersDialogState extends State<_RemindersDialog> {
  List<Reminder> get _reminders => [
    for (final reminder in widget.library.reminders)
      if (reminder.noteId == widget.note.id) reminder,
  ];

  Future<void> _edit([Reminder? reminder]) async {
    final draft = await showDialog<_ReminderDraft>(
      context: context,
      builder: (context) => _ReminderEditor(reminder: reminder),
    );
    if (draft == null || !mounted) return;
    // Permission is asked by the initiating gesture, never as a surprise when
    // an account update arrives from somebody's other device. A refusal does
    // not abort the write: it is still valuable on the next device or after
    // the person enables notifications in Settings.
    unawaited(ReminderScheduler.instance?.requestPermission().then((_) {}));
    final timezone = reminder?.timezone ?? await deviceTimezone();
    final saved = Reminder(
      id: reminder?.id ?? '',
      noteId: widget.note.id,
      fireAt: draft.when.toUtc(),
      repeat: draft.repeat,
      timezone: timezone,
    );
    if (reminder == null) {
      await widget.library.createReminder(
        noteId: saved.noteId,
        fireAt: saved.fireAt,
        repeat: saved.repeat,
        timezone: saved.timezone,
      );
    } else {
      await widget.library.setReminder(saved);
    }
    if (mounted) setState(() {});
  }

  Future<void> _delete(Reminder reminder) async {
    await widget.library.forgetReminder(reminder.id);
    if (mounted) setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final localizations = MaterialLocalizations.of(context);
    final reminders = _reminders;
    return AlertDialog(
      key: const Key('reminders-dialog'),
      title: const Text('Reminders'),
      content: SizedBox(
        width: 360,
        child: reminders.isEmpty
            ? const Text('No reminders for this note.')
            : ListView(
                shrinkWrap: true,
                children: [
                  for (final reminder in reminders)
                    ListTile(
                      key: Key('reminder-${reminder.id}'),
                      contentPadding: EdgeInsets.zero,
                      title: Text(
                        '${localizations.formatFullDate(reminder.fireAt.toLocal())} '
                        '${localizations.formatTimeOfDay(TimeOfDay.fromDateTime(reminder.fireAt.toLocal()))}',
                      ),
                      subtitle: Text(_repeatLabel(reminder.repeat)),
                      onTap: () => _edit(reminder),
                      trailing: IconButton(
                        key: Key('forget-reminder-${reminder.id}'),
                        icon: const Icon(Icons.delete_outline),
                        tooltip: 'Delete reminder',
                        onPressed: () => _delete(reminder),
                      ),
                    ),
                ],
              ),
      ),
      actions: [
        TextButton(
          key: const Key('close-reminders'),
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Close'),
        ),
        FilledButton.icon(
          key: const Key('add-reminder'),
          onPressed: _edit,
          icon: const Icon(Icons.add_alert_outlined),
          label: const Text('Add reminder'),
        ),
      ],
    );
  }
}

class _ReminderDraft {
  const _ReminderDraft({required this.when, required this.repeat});

  final DateTime when;
  final ReminderRepeat repeat;
}

class _ReminderEditor extends StatefulWidget {
  const _ReminderEditor({this.reminder});

  final Reminder? reminder;

  @override
  State<_ReminderEditor> createState() => _ReminderEditorState();
}

class _ReminderEditorState extends State<_ReminderEditor> {
  late DateTime _when = (widget.reminder?.fireAt.toLocal() ?? DateTime.now())
      .add(widget.reminder == null ? const Duration(hours: 1) : Duration.zero);
  late ReminderRepeat _repeat = widget.reminder?.repeat ?? ReminderRepeat.never;

  Future<void> _date() async {
    final chosen = await showDatePicker(
      context: context,
      initialDate: _when,
      firstDate: DateTime.now().subtract(const Duration(days: 1)),
      lastDate: DateTime.now().add(const Duration(days: 3650)),
    );
    if (chosen != null && mounted) {
      setState(
        () => _when = DateTime(
          chosen.year,
          chosen.month,
          chosen.day,
          _when.hour,
          _when.minute,
        ),
      );
    }
  }

  Future<void> _time() async {
    final chosen = await showTimePicker(
      context: context,
      initialTime: TimeOfDay.fromDateTime(_when),
    );
    if (chosen != null && mounted) {
      setState(
        () => _when = DateTime(
          _when.year,
          _when.month,
          _when.day,
          chosen.hour,
          chosen.minute,
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final localizations = MaterialLocalizations.of(context);
    return AlertDialog(
      key: const Key('reminder-editor'),
      title: Text(widget.reminder == null ? 'Add reminder' : 'Edit reminder'),
      scrollable: true,
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ListTile(
            key: const Key('reminder-date'),
            leading: const Icon(Icons.calendar_today_outlined),
            title: Text(localizations.formatFullDate(_when)),
            onTap: _date,
          ),
          ListTile(
            key: const Key('reminder-time'),
            leading: const Icon(Icons.schedule_outlined),
            title: Text(
              localizations.formatTimeOfDay(TimeOfDay.fromDateTime(_when)),
            ),
            onTap: _time,
          ),
          DropdownButtonFormField<ReminderRepeat>(
            key: const Key('reminder-repeat'),
            initialValue: _repeat,
            decoration: InputDecoration(
              labelText: 'Repeat',
              helperText:
                  _repeat == ReminderRepeat.monthly ||
                      _repeat == ReminderRepeat.yearly
                  ? 'Dates missing from the calendar are skipped.'
                  : null,
              helperMaxLines: 2,
            ),
            items: [
              for (final repeat in ReminderRepeat.values)
                DropdownMenuItem(
                  value: repeat,
                  child: Text(_repeatLabel(repeat)),
                ),
            ],
            onChanged: (repeat) {
              if (repeat != null) setState(() => _repeat = repeat);
            },
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('save-reminder'),
          onPressed: () => Navigator.of(
            context,
          ).pop(_ReminderDraft(when: _when, repeat: _repeat)),
          child: const Text('Save'),
        ),
      ],
    );
  }
}

String _repeatLabel(ReminderRepeat repeat) => switch (repeat) {
  ReminderRepeat.never => 'Does not repeat',
  ReminderRepeat.daily => 'Daily',
  ReminderRepeat.weekly => 'Weekly',
  ReminderRepeat.monthly => 'Monthly',
  ReminderRepeat.yearly => 'Yearly',
};

class _Swatch extends StatelessWidget {
  const _Swatch({
    required this.colour,
    required this.chosen,
    required this.onTap,
  });

  final NoteColour? colour;
  final bool chosen;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Semantics(
      button: true,
      selected: chosen,
      label: colour == null ? 'No colour' : '${colour!.name} colour',
      child: Tooltip(
        message: colour == null ? 'No colour' : colour!.name,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 8),
          child: InkWell(
            key: Key('swatch-${colour?.name ?? 'none'}'),
            customBorder: const CircleBorder(),
            onTap: onTap,
            child: Container(
              width: 48,
              height: 48,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color:
                    noteColourTint(context, colour) ??
                    theme.colorScheme.surface,
                border: Border.all(
                  color: chosen
                      ? theme.colorScheme.primary
                      : theme.dividerColor,
                  width: chosen ? 3 : 1,
                ),
              ),
              child: colour == null
                  ? const Icon(Icons.format_color_reset_outlined, size: 20)
                  : null,
            ),
          ),
        ),
      ),
    );
  }
}

/// A grid of cards of different heights, in the shortest column each time.
///
/// Hand-rolled, and the reason is worth writing down: a real masonry measures
/// its children, and measuring means a layout pass per card before anything is
/// painted. A note card's height is a function of what is in the summary — a
/// title and up to four preview lines — so it can be *estimated* exactly well
/// enough to balance the columns, with no measurement and no package. The cost
/// is that columns can end up slightly uneven; the alternative costs a frame.
///
/// While a card is being carried the balancing pauses: see [dragOrder].
class _MasonryGrid extends StatelessWidget {
  const _MasonryGrid({
    required this.wrap,
    required this.children,
    this.draggedId,
    this.targetId,
    this.dragOrder,
  });

  /// The cards, unwrapped, because the estimate below reads what is on them.
  /// While a card is being carried this is the preview order: the carried card
  /// is in it, at the place it would land, and draws itself as a gap.
  final List<_NoteCard> children;

  /// What actually goes in the column: the card, or the card made draggable.
  /// Kept apart from [children] so that balancing the columns stays a question
  /// about a note's contents and not about what is wrapped around it.
  final Widget Function(_NoteCard) wrap;

  /// The card under the finger, if any.
  final String? draggedId;

  /// The card it would land on, if any.
  final String? targetId;

  /// The order the notes were in when the drag began.
  ///
  /// While a card is carried the grid stops balancing itself and works from
  /// this order instead: every other card stays in the column it was in, so
  /// only the gap the card left and the gap it will fill move. Re-running the
  /// balancing on each preview would shuffle cards between columns under the
  /// finger, and the card under the finger is what decides the next target.
  final List<NoteSummary>? dragOrder;

  /// Roughly what a card will take: the padding and title, a line per preview
  /// item, and the matched line when it is a search result. Units are arbitrary
  /// and only ever compared with each other.
  static double _estimate(NoteSummary note, String? matched) =>
      64 +
      (note.title.isEmpty ? 0 : 28) +
      note.preview.length * 22 +
      (matched == null ? 0 : 22) +
      // One row of chips, whatever the number: they wrap, and a card with eight
      // labels on it is rarer than the error in guessing how many rows they take.
      (note.tags.isEmpty ? 0 : 30);

  /// The column each note sat in, worked out the way the grid would balance
  /// itself. Used only while dragging, to hold the columns still.
  static Map<String, int> _columnsAt(int columns, List<NoteSummary> order) {
    final heights = List.filled(columns, 0.0);
    final columnOf = <String, int>{};
    for (final note in order) {
      var shortest = 0;
      for (var i = 1; i < columns; i++) {
        if (heights[i] < heights[shortest]) shortest = i;
      }
      columnOf[note.id] = shortest;
      heights[shortest] += _estimate(note, null);
    }
    return columnOf;
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        // Phones use two columns so grid view remains distinct from the list,
        // including on narrower handsets. Wider layouts add columns at roughly
        // 260 logical pixels each.
        final columns = constraints.maxWidth < 600
            ? 2
            : (constraints.maxWidth / 260).floor().clamp(2, 4);
        final buckets = List.generate(columns, (_) => <Widget>[]);
        Widget brick(_NoteCard card) => Padding(
          padding: const EdgeInsets.only(bottom: 8),
          child: wrap(card),
        );

        final dragged = draggedId;
        final order = dragOrder;
        if (dragged != null && order != null) {
          final columnOf = _columnsAt(columns, order);
          final landing = targetId;
          int columnFor(_NoteCard card) {
            final id = card.note.id;
            if (id != dragged) return (columnOf[id] ?? 0).clamp(0, columns - 1);
            // The carried card goes where it would land, and the card it left
            // behind closes up. Everyone else stays put.
            final destination = landing == null ? null : columnOf[landing];
            return (destination ?? columnOf[id] ?? 0).clamp(0, columns - 1);
          }

          for (final card in children) {
            buckets[columnFor(card)].add(brick(card));
          }
        } else {
          final heights = List.filled(columns, 0.0);
          for (final card in children) {
            var shortest = 0;
            for (var i = 1; i < columns; i++) {
              if (heights[i] < heights[shortest]) shortest = i;
            }
            buckets[shortest].add(brick(card));
            heights[shortest] += _estimate(card.note, card.matched);
          }
        }
        return SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              for (var i = 0; i < columns; i++) ...[
                if (i > 0) const SizedBox(width: 8),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: buckets[i],
                  ),
                ),
              ],
            ],
          ),
        );
      },
    );
  }
}

/// The card as it looks when nobody is touching it: no highlight, no hidden
/// state, no gestures.
///
/// Drawn inside the surface the expand route grows, so the thing that grows
/// carries what the person tapped rather than a flat colour. Built with the same
/// [Card] chrome as the real one so the hand-over at either end of the trip is
/// invisible.
Widget _noteCardCopy(
  BuildContext context,
  NoteSummary note, {
  required bool hasReminder,
  CoverScopeData? covers,
}) {
  final card = Card(
    margin: EdgeInsets.zero,
    clipBehavior: Clip.antiAlias,
    color: noteColourTint(context, note.colour),
    child: _NoteCardFace(note: note, hasReminder: hasReminder),
  );
  if (covers == null) return card;
  return CoverScopeData(
    read: covers.read,
    revision: covers.revision,
    child: card,
  );
}

/// How far a card shrinks under a finger, so the press is felt before the tap
/// is answered.
const _kPressedScale = .975;

/// The rectangle a card is drawn in at the moment its finger leaves it.
///
/// The press scale is a transform inside the card, so the box it is measured in
/// is the unscaled one. Opening from that box would start the trip one step
/// larger than the card on screen, which is a jump on the first frame of the
/// very animation meant to explain where the card went.
Rect? _asPressed(Rect? rect) => rect == null
    ? null
    : Rect.fromCenter(
        center: rect.center,
        width: rect.width * _kPressedScale,
        height: rect.height * _kPressedScale,
      );

/// One note, as a card: what it is called, what is on it, how much is done, and
/// what this person has made of it.
class _NoteCard extends StatefulWidget {
  const _NoteCard({
    super.key,
    required this.note,
    required this.hasReminder,
    required this.onTap,
    this.onHold,
    this.matched,
    this.hidden = false,
    this.selected = false,
  });

  final NoteSummary note;
  final bool hasReminder;
  final VoidCallback onTap;
  final bool hidden;

  /// Selecting the card, when the card is not draggable. Null when it is: there
  /// the long press is the drag, and [_DraggableCard] decides whether it was a
  /// press after all.
  final VoidCallback? onHold;

  /// Whether the card is in the selection: outlined and ticked, at the same
  /// size, so picking cards never moves the grid.
  final bool selected;

  /// The line a search matched, when this card is a result. Shown because the
  /// preview is four items and the match may be the twelfth.
  final String? matched;

  @override
  State<_NoteCard> createState() => _NoteCardState();
}

class _NoteCardState extends State<_NoteCard> {
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    final note = widget.note;
    return ExcludeSemantics(
      excluding: widget.hidden,
      child: IgnorePointer(
        ignoring: widget.hidden,
        child: Opacity(
          key: Key('note-card-cover:${note.id}'),
          // Not drawn while its own screen is open: the screen above is this
          // card, grown. It keeps its place so the way back has a landing spot.
          opacity: widget.hidden ? 0 : 1,
          child: AnimatedScale(
            scale: _pressed ? _kPressedScale : 1,
            duration: EasyMotion.duration(context, EasyMotion.fast),
            curve: EasyMotion.enter,
            child: Semantics(
              selected: widget.selected,
              child: Stack(
                fit: StackFit.passthrough,
                children: [
                  Card(
                    margin: EdgeInsets.zero,
                    clipBehavior: Clip.antiAlias,
                    color: noteColourTint(context, note.colour),
                    child: InkWell(
                      key: Key('note:${note.id}'),
                      onTap: widget.onTap,
                      onLongPress: widget.onHold,
                      onHighlightChanged: (pressed) {
                        if (_pressed != pressed) {
                          setState(() => _pressed = pressed);
                        }
                      },
                      child: _NoteCardFace(
                        note: note,
                        hasReminder: widget.hasReminder,
                        matched: widget.matched,
                      ),
                    ),
                  ),
                  Positioned.fill(
                    child: IgnorePointer(
                      child: _SelectedMark(selected: widget.selected),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// What a selected card wears over itself: an outline in the accent colour and
/// a tick in the corner. Drawn on top rather than as the card's border, so a
/// card is the same size picked or not and the masonry never shifts under it.
class _SelectedMark extends StatelessWidget {
  const _SelectedMark({required this.selected});

  final bool selected;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final shape = theme.cardTheme.shape;
    final radius = shape is RoundedRectangleBorder
        ? shape.borderRadius
        : BorderRadius.circular(16);
    return AnimatedOpacity(
      key: selected ? const Key('card-selected') : null,
      opacity: selected ? 1 : 0,
      duration: EasyMotion.duration(context, EasyMotion.fast),
      curve: EasyMotion.enter,
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: radius,
          border: Border.all(color: scheme.primary, width: 3),
        ),
        child: Align(
          alignment: Alignment.topRight,
          child: Padding(
            padding: const EdgeInsets.all(6),
            child: DecoratedBox(
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: scheme.primary,
              ),
              child: Padding(
                padding: const EdgeInsets.all(2),
                child: Icon(Icons.check, size: 16, color: scheme.onPrimary),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// What is on a card, without the card: the cover, the title, the preview and
/// the chips.
///
/// Split out from [_NoteCard] because the card is drawn in two places at once
/// while a note opens: on the list, and inside the surface growing out of it.
/// The two have to line up to the pixel, and the only way to be sure of that is
/// for both to be this widget.
class _NoteCardFace extends StatelessWidget {
  const _NoteCardFace({
    required this.note,
    required this.hasReminder,
    this.matched,
  });

  final NoteSummary note;
  final bool hasReminder;

  /// The line a search matched, when this card is a result.
  final String? matched;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final isList = note.kind == NoteKind.checklist;
    // Only a list counts what it is not showing: a text note's preview is lines
    // of prose, and "+3 more" under a paragraph would be counting items it
    // does not have.
    final notShown = isList ? note.items - note.preview.length : 0;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (note.cover case final cover?)
          NoteCover(
            noteId: note.id,
            attachment: cover,
            width: note.coverWidth,
            height: note.coverHeight,
          ),
        Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (note.title.isNotEmpty || note.pinned)
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (note.title.isNotEmpty)
                      Expanded(
                        child: Text(
                          note.title,
                          style: theme.textTheme.titleMedium,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                        ),
                      )
                    else
                      const Spacer(),
                    if (note.pinned)
                      Icon(
                        Icons.push_pin,
                        key: const Key('pinned'),
                        size: 16,
                        color: theme.textTheme.bodySmall?.color,
                      ),
                  ],
                ),
              if (note.title.isNotEmpty && note.preview.isNotEmpty)
                const SizedBox(height: 6),
              for (final text in note.preview)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 1),
                  child: Text(
                    isList ? '• $text' : text,
                    style: theme.textTheme.bodyMedium,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              if (notShown > 0)
                Padding(
                  padding: const EdgeInsets.only(top: 4),
                  child: Text(
                    '+$notShown more',
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              if (note.title.isEmpty && note.preview.isEmpty)
                Text(
                  isList ? 'Untitled checklist' : 'Empty note',
                  style: theme.textTheme.titleMedium?.copyWith(
                    color: theme.disabledColor,
                  ),
                ),
              if (isList && note.items > 0) ...[
                const SizedBox(height: 8),
                Text(
                  '${note.checked} of ${note.items} done',
                  style: theme.textTheme.bodySmall,
                ),
              ],
              if (note.images > 0 || note.audio > 0 || hasReminder) ...[
                const SizedBox(height: 8),
                _NoteIndicators(
                  images: note.images,
                  audio: note.audio,
                  hasReminder: hasReminder,
                ),
              ],
              // The labels, as small chips. On the card and not only in the
              // drawer, because a note's filing is part of what it is — and
              // because a shared list that one person has labelled looks the
              // same to everyone else, which is the whole point of the account
              // document.
              if (note.archived && !note.trashed) ...[
                const SizedBox(height: 8),
                const _Chip(label: 'Archived'),
              ],
              if (note.tags.isNotEmpty) ...[
                const SizedBox(height: 8),
                Wrap(
                  spacing: 4,
                  runSpacing: 4,
                  children: [
                    for (final tag in note.tags)
                      _Chip(
                        // Keyed on the name, because that is what a test and a
                        // person both look for; the label's identity is on the
                        // summary and is what every gesture uses.
                        key: Key('chip:${note.id}:${tag.name}'),
                        label: tag.name,
                      ),
                  ],
                ),
              ],
              // Only when the match is not already on the card: a hit on the
              // title, or on one of the four items the preview shows, needs no
              // second copy of itself underneath.
              if (matched != null &&
                  matched != note.title &&
                  !note.preview.contains(matched)) ...[
                const SizedBox(height: 6),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(
                      Icons.search,
                      size: 14,
                      color: theme.textTheme.bodySmall?.color,
                    ),
                    const SizedBox(width: 4),
                    Expanded(
                      child: Text(
                        matched!,
                        key: Key('matched:${note.id}'),
                        style: theme.textTheme.bodySmall,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                  ],
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }
}

/// One label on a card. Small, flat and not tappable: a card's own gesture is
/// opening the note, and a chip that could be pressed would be a second target
/// inside the first.
class _Chip extends StatelessWidget {
  const _Chip({super.key, required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(10),
        // Over whatever colour the card is, so a chip reads on all ten of them.
        color: theme.colorScheme.onSurface.withValues(alpha: 0.08),
      ),
      child: Text(
        label,
        style: theme.textTheme.labelSmall,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
    );
  }
}

/// Small indicators for the media and personal reminders attached to a note.
class _NoteIndicators extends StatelessWidget {
  const _NoteIndicators({
    required this.images,
    required this.audio,
    required this.hasReminder,
  });

  final int images;
  final int audio;
  final bool hasReminder;

  @override
  Widget build(BuildContext context) {
    final style = Theme.of(context).textTheme.bodySmall;
    Widget count(IconData icon, int n) => Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 16, color: style?.color),
        const SizedBox(width: 2),
        Text('$n', style: style),
      ],
    );
    final said = [
      if (images > 0) images == 1 ? '1 image' : '$images images',
      if (audio > 0) audio == 1 ? '1 sound file' : '$audio sound files',
      if (hasReminder) 'Has reminder',
    ].join(', ');
    return Semantics(
      label: said,
      excludeSemantics: true,
      child: Wrap(
        key: const Key('media-count'),
        spacing: 12,
        children: [
          if (images > 0) count(Icons.image_outlined, images),
          if (audio > 0) count(Icons.audiotrack_outlined, audio),
          if (hasReminder)
            Icon(
              Icons.notifications_outlined,
              key: const Key('reminder-indicator'),
              size: 16,
              color: style?.color,
            ),
        ],
      ),
    );
  }
}

/// The kinds of note that can be made, revealed from the bottom up.
///
/// It is mounted only while the menu is open and animates itself in: one
/// controller drives a short wave, each type fading and rising in turn, so the
/// type nearest the button leads and the one furthest away follows.
class _NoteTypeMenu extends StatefulWidget {
  const _NoteTypeMenu({
    required this.onCreateText,
    required this.onCreateList,
    required this.onCreateAudio,
  });

  final VoidCallback onCreateText;
  final VoidCallback onCreateList;
  final VoidCallback onCreateAudio;

  @override
  State<_NoteTypeMenu> createState() => _NoteTypeMenuState();
}

class _NoteTypeMenuState extends State<_NoteTypeMenu>
    with SingleTickerProviderStateMixin {
  /// The gap between one type arriving and the next, as a fraction of the
  /// controller's run, and how much of that run one type takes.
  static const _stagger = 0.16;
  static const _span = 0.5;
  static const _wave = Duration(milliseconds: 320);

  late final AnimationController _controller;
  late final Animation<double> _textEntrance;
  late final Animation<double> _listEntrance;
  late final Animation<double> _audioEntrance;
  bool _started = false;

  @override
  void initState() {
    super.initState();
    _controller = AnimationController(vsync: this, duration: _wave);
    _textEntrance = _entrance(0);
    _listEntrance = _entrance(1);
    _audioEntrance = _entrance(2);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_started) return;
    _started = true;
    // A reduced-motion setting takes the wave away, not the types.
    _controller.duration = EasyMotion.duration(context, _wave);
    _controller.forward();
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Animation<double> _entrance(int order) {
    final start = order * _stagger;
    return CurvedAnimation(
      parent: _controller,
      curve: Interval(
        start,
        (start + _span).clamp(0.0, 1.0),
        curve: EasyMotion.enter,
      ),
    );
  }

  Widget _type({
    required Key key,
    required String heroTag,
    required String label,
    required IconData icon,
    required Animation<double> entrance,
    required VoidCallback onPressed,
  }) {
    return FadeTransition(
      opacity: entrance,
      child: SlideTransition(
        position: Tween<Offset>(
          begin: const Offset(0, 0.35),
          end: Offset.zero,
        ).animate(entrance),
        child: Padding(
          padding: const EdgeInsets.only(bottom: 12),
          child: FloatingActionButton.extended(
            key: key,
            heroTag: heroTag,
            tooltip: label,
            onPressed: onPressed,
            icon: Icon(icon),
            label: Text(label),
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return ClipRect(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          // Top to bottom: audio, list, text. The wave rises from the text
          // type nearest the button, so its order is the last.
          _type(
            key: const Key('new-audio-note-button'),
            heroTag: 'new-audio-note',
            label: 'Audio',
            icon: Icons.mic_none,
            entrance: _audioEntrance,
            onPressed: widget.onCreateAudio,
          ),
          _type(
            key: const Key('new-list-button'),
            heroTag: 'new-list',
            label: 'List',
            icon: Icons.check_box_outlined,
            entrance: _listEntrance,
            onPressed: widget.onCreateList,
          ),
          _type(
            key: const Key('new-text-note-button'),
            heroTag: 'new-text-note',
            label: 'Text',
            icon: Icons.notes_outlined,
            entrance: _textEntrance,
            onPressed: widget.onCreateText,
          ),
        ],
      ),
    );
  }
}

/// The button that opens the note types, and the label that says what it makes.
///
/// Pressed, the plus turns into a cross while the label is clipped away, so it
/// becomes a bare button that folds the types back up again.
class _CreateNoteButton extends StatelessWidget {
  const _CreateNoteButton({required this.expanded, required this.onPressed});

  final bool expanded;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Tooltip(
      message: expanded ? 'Hide note types' : 'Create note',
      child: Material(
        color: scheme.primary,
        elevation: 3,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(18)),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          key: const Key('new-note-button'),
          onTap: onPressed,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 16),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                // Rotating a plus by an eighth of a turn is how it becomes a
                // cross, so the icon morphs rather than being swapped.
                AnimatedRotation(
                  turns: expanded ? 0.125 : 0,
                  duration: EasyMotion.fast,
                  curve: EasyMotion.enter,
                  child: Icon(Icons.add, color: scheme.onPrimary),
                ),
                // The label keeps its width and is clipped from the right as
                // the button contracts, so it disappears with the plus.
                ClipRect(
                  child: TweenAnimationBuilder<double>(
                    tween: Tween(begin: 1, end: expanded ? 0 : 1),
                    duration: EasyMotion.fast,
                    curve: EasyMotion.enter,
                    builder: (context, value, child) => Align(
                      alignment: Alignment.centerLeft,
                      widthFactor: value,
                      child: child,
                    ),
                    child: Padding(
                      padding: const EdgeInsets.only(left: 8),
                      child: Text(
                        'Create note',
                        style: theme.textTheme.labelLarge?.copyWith(
                          color: scheme.onPrimary,
                        ),
                      ),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
