/// Where a person becomes one person across their devices, or finds out they
/// already are.
///
/// There is no login here and there must not be one ([ADR
/// 0013](../../../docs/adr/0013-recovery-phrase.md)). A person is twelve words:
/// this screen mints them, shows them once, and does not let the moment pass
/// until they have been written down — or takes twelve words typed on another
/// device and makes this one the same person's.
///
/// The backup check is the reason this is three widgets and not one dialog. The
/// words are not stored anywhere, so the screen that shows them is the only
/// place they will ever exist, and an account whose phrase nobody copied is the
/// one state this design cannot recover from.
library;

import 'dart:math';

import 'package:flutter/material.dart';

import '../core/note_library.dart';

/// How many words the backup check asks for.
///
/// Three rather than all twelve: the check is there to catch a person who
/// tapped past the words without copying them, and three chosen at random does
/// that as surely as twelve do, without making them type out the whole phrase
/// a minute after reading it.
const backupCheckWords = 3;

/// Whether this device belongs to an account, and the two ways to make it.
class AccountScreen extends StatefulWidget {
  const AccountScreen({super.key, required this.library, this.ask});

  final NoteLibrary library;

  /// Which words the backup check asks for, 1-based. Random when null, which
  /// is always outside a test.
  final List<int>? ask;

  @override
  State<AccountScreen> createState() => _AccountScreenState();
}

class _AccountScreenState extends State<AccountScreen> {
  /// A gesture is in flight, so neither button may start a second one. Minting
  /// twice would throw, and adopting while minting would be worse.
  bool _busy = false;

  Future<void> _create() async {
    setState(() => _busy = true);
    final List<String> words;
    try {
      words = await widget.library.createAccount();
    } catch (e) {
      if (!mounted) return;
      setState(() => _busy = false);
      _say('$e');
      return;
    }
    if (!mounted) return;
    // From here the account exists and the words are only in this variable.
    // The route below is where they are shown and where they die.
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (context) =>
            RecoveryPhraseScreen(words: words, ask: widget.ask),
      ),
    );
    if (!mounted) return;
    setState(() => _busy = false);
  }

  Future<void> _adopt() async {
    final adopted = await Navigator.of(context).push<bool>(
      MaterialPageRoute<bool>(
        builder: (context) => AdoptAccountScreen(library: widget.library),
      ),
    );
    if (!mounted) return;
    setState(() {});
    if (adopted == true) {
      // Nothing is on screen yet, and that has to be said, or the first thing
      // a person sees after typing twelve words carefully is an empty grid.
      _say(
        'This device is now yours. Your notes arrive when it reaches the relay.',
      );
    }
  }

  void _say(String message) {
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(message, key: const Key('account-message'))),
    );
  }

  @override
  Widget build(BuildContext context) {
    final has = widget.library.hasAccount;
    return Scaffold(
      appBar: AppBar(title: const Text('Your devices')),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: has
            ? [
                Icon(Icons.devices, size: 48),
                SizedBox(height: 16),
                Text(
                  'This device belongs to your account.',
                  key: Key('account-yes'),
                  style: Theme.of(context).textTheme.titleMedium,
                ),
                SizedBox(height: 12),
                // It cannot offer to show the words again, and says why, so
                // that nobody goes looking for the button.
                Text(
                  'Your notes, colours, labels and pins follow you to any '
                  'device you set up with your recovery phrase. The phrase is '
                  'not kept anywhere, so it cannot be shown again — the copy '
                  'you wrote down is the only one.',
                ),
              ]
            : [
                const Icon(Icons.phone_android, size: 48),
                const SizedBox(height: 16),
                Text(
                  'This device is on its own.',
                  key: Key('account-no'),
                  style: Theme.of(context).textTheme.titleMedium,
                ),
                const SizedBox(height: 12),
                const Text(
                  'Set up an account to have your notes on your other devices '
                  'too. There is no password and no email: your account is '
                  'twelve words, which you write down once. Anyone with the '
                  'words has your notes, and without them nobody can get them '
                  'back — not even us.',
                ),
                const SizedBox(height: 24),
                FilledButton.icon(
                  key: const Key('create-account-button'),
                  icon: const Icon(Icons.key),
                  label: const Text('Set up an account'),
                  onPressed: _busy ? null : _create,
                ),
                const SizedBox(height: 12),
                OutlinedButton.icon(
                  key: const Key('adopt-account-button'),
                  icon: const Icon(Icons.edit_note),
                  label: const Text('I already have twelve words'),
                  onPressed: _busy ? null : _adopt,
                ),
              ],
      ),
    );
  }
}

/// The twelve words, once, and then the check that they were written down.
///
/// Leaving before the check passes is possible — a screen with no way out is a
/// trap, and a person may genuinely need to — but not by accident: the back
/// gesture asks first, and says in so many words that the phrase is gone after
/// this. What cannot happen is the quiet version, a tap past the words that
/// looks like finishing.
class RecoveryPhraseScreen extends StatefulWidget {
  const RecoveryPhraseScreen({super.key, required this.words, this.ask});

  final List<String> words;

  /// Which words to ask for, 1-based. Random when null.
  final List<int>? ask;

  @override
  State<RecoveryPhraseScreen> createState() => _RecoveryPhraseScreenState();
}

class _RecoveryPhraseScreenState extends State<RecoveryPhraseScreen> {
  bool _checking = false;
  bool _passed = false;

  /// Chosen once per visit to the screen, not per attempt: re-rolling after a
  /// wrong answer would reward guessing over going back to the paper.
  late final List<int> _ask = widget.ask ?? _pick(widget.words.length);

  late final _fields = [
    for (var i = 0; i < _ask.length; i++) TextEditingController(),
  ];

  /// Which asked-for words were wrong on the last try, by index into [_ask].
  Set<int> _wrong = {};

  static List<int> _pick(int of) {
    final positions = [for (var i = 1; i <= of; i++) i]
      ..shuffle(Random.secure());
    return positions.take(backupCheckWords).toList()..sort();
  }

  @override
  void dispose() {
    for (final field in _fields) {
      field.dispose();
    }
    super.dispose();
  }

  void _check() {
    // Forgiving about case and spacing, like the core is when adopting: the
    // check is about whether the words are on paper, not about typing.
    final wrong = {
      for (var i = 0; i < _ask.length; i++)
        if (_fields[i].text.trim().toLowerCase() !=
            widget.words[_ask[i] - 1].toLowerCase())
          i,
    };
    if (wrong.isEmpty) {
      setState(() => _passed = true);
      Navigator.of(context).pop();
      return;
    }
    setState(() => _wrong = wrong);
  }

  void _showAgain() {
    for (final field in _fields) {
      field.clear();
    }
    setState(() {
      _wrong = {};
      _checking = false;
    });
  }

  /// The one way out that is not the check, and it has to be chosen twice.
  Future<void> _leave() async {
    final sure = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('phrase-leave-dialog'),
        title: const Text('Leave without writing it down?'),
        content: const Text(
          'These words will not be shown again. Without them, your notes '
          'cannot be moved to another device or brought back if this one is '
          'lost.',
        ),
        actions: [
          FilledButton(
            key: const Key('phrase-stay'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Back to the words'),
          ),
          TextButton(
            key: const Key('phrase-leave'),
            style: TextButton.styleFrom(
              foregroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Leave anyway'),
          ),
        ],
      ),
    );
    if (sure != true || !mounted) return;
    setState(() => _passed = true);
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      canPop: _passed,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _leave();
      },
      child: Scaffold(
        appBar: AppBar(
          title: Text(_checking ? 'Check your words' : 'Your recovery phrase'),
        ),
        body: _checking ? _checkBody() : _wordsBody(),
      ),
    );
  }

  Widget _wordsBody() {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const Text(
          'Write these twelve words down, in order, on paper. They are the '
          'only way to put your notes on another device, and they will not be '
          'shown again.',
        ),
        const SizedBox(height: 20),
        // Two columns of six, numbered, so that a word copied into the wrong
        // line is visible on the paper as well as here.
        GridView.count(
          key: const Key('phrase-words'),
          crossAxisCount: 2,
          shrinkWrap: true,
          physics: const NeverScrollableScrollPhysics(),
          childAspectRatio: 4,
          mainAxisSpacing: 8,
          crossAxisSpacing: 8,
          children: [
            for (var i = 0; i < widget.words.length; i++)
              Container(
                key: Key('phrase-word-${i + 1}'),
                padding: const EdgeInsets.symmetric(horizontal: 12),
                alignment: Alignment.centerLeft,
                decoration: BoxDecoration(
                  color: theme.colorScheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Text.rich(
                  TextSpan(
                    children: [
                      TextSpan(
                        text: '${i + 1}. ',
                        style: TextStyle(color: theme.colorScheme.outline),
                      ),
                      TextSpan(
                        text: widget.words[i],
                        style: const TextStyle(
                          fontSize: 16,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
          ],
        ),
        const SizedBox(height: 16),
        const Text(
          'Anyone who has these words has your notes. Do not photograph them '
          'or keep them in another app.',
        ),
        const SizedBox(height: 24),
        FilledButton(
          key: const Key('phrase-written'),
          onPressed: () => setState(() => _checking = true),
          child: const Text('I have written them down'),
        ),
      ],
    );
  }

  Widget _checkBody() {
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const Text(
          'From what you wrote down — not from memory — type the words asked '
          'for below.',
        ),
        const SizedBox(height: 20),
        for (var i = 0; i < _ask.length; i++) ...[
          TextField(
            key: Key('check-word-${_ask[i]}'),
            controller: _fields[i],
            autofocus: i == 0,
            autocorrect: false,
            enableSuggestions: false,
            textInputAction: i == _ask.length - 1
                ? TextInputAction.done
                : TextInputAction.next,
            onSubmitted: i == _ask.length - 1 ? (_) => _check() : null,
            decoration: InputDecoration(
              labelText: 'Word ${_ask[i]}',
              border: const OutlineInputBorder(),
              errorText: _wrong.contains(i)
                  ? 'That is not word ${_ask[i]}'
                  : null,
            ),
          ),
          const SizedBox(height: 12),
        ],
        const SizedBox(height: 12),
        FilledButton(
          key: const Key('check-confirm'),
          onPressed: _check,
          child: const Text('Check'),
        ),
        const SizedBox(height: 8),
        // Back to the words rather than a hint: a person who got one wrong has
        // a wrong word on paper, and the fix is to look at the list again.
        TextButton(
          key: const Key('check-show-again'),
          onPressed: _showAgain,
          child: const Text('Show the words again'),
        ),
      ],
    );
  }
}

/// Twelve words typed on a device that does not know them yet.
///
/// One field rather than twelve: people copy a phrase from paper line by line
/// and a paste from a password manager arrives as one string, and a single
/// field serves both. The core is the judge of whether the words are a phrase —
/// it knows the wordlist and the checksum — so all this counts is how many.
class AdoptAccountScreen extends StatefulWidget {
  const AdoptAccountScreen({super.key, required this.library});

  final NoteLibrary library;

  @override
  State<AdoptAccountScreen> createState() => _AdoptAccountScreenState();
}

class _AdoptAccountScreenState extends State<AdoptAccountScreen> {
  final _controller = TextEditingController();
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  int get _count =>
      _controller.text.split(RegExp(r'\s+')).where((w) => w.isNotEmpty).length;

  Future<void> _submit() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.library.adoptAccount(_controller.text.trim());
    } catch (e) {
      if (!mounted) return;
      // The core's own words: it is the one that knows whether the checksum
      // failed, and "check for a swap or a typo" is more use than "invalid".
      setState(() {
        _busy = false;
        _error = _message(e);
      });
      return;
    }
    if (!mounted) return;
    Navigator.of(context).pop(true);
  }

  /// What went wrong, without the exception's class name in front of it.
  static String _message(Object e) => switch (e) {
    ArgumentError(:final message) => '$message',
    StateError(:final message) => message,
    _ => '$e',
  };

  @override
  Widget build(BuildContext context) {
    final count = _count;
    return Scaffold(
      appBar: AppBar(title: const Text('Use your recovery phrase')),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: [
          const Text(
            'Type the twelve words you wrote down when you set up your '
            'account, in order. The notes already on this device are kept.',
          ),
          const SizedBox(height: 20),
          TextField(
            key: const Key('adopt-phrase-field'),
            controller: _controller,
            autofocus: true,
            autocorrect: false,
            enableSuggestions: false,
            minLines: 3,
            maxLines: 4,
            keyboardType: TextInputType.visiblePassword,
            decoration: InputDecoration(
              hintText: 'twelve words, separated by spaces',
              border: const OutlineInputBorder(),
              helperText: '$count of 12 words',
              errorText: _error,
              errorMaxLines: 3,
            ),
            onChanged: (_) => setState(() => _error = null),
          ),
          const SizedBox(height: 24),
          FilledButton(
            key: const Key('adopt-confirm'),
            onPressed: count == 12 && !_busy ? _submit : null,
            child: const Text('Use these words'),
          ),
        ],
      ),
    );
  }
}
