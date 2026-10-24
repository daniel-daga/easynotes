/// The first run: what the app is, how its one optional setup works, and a
/// send-off.
///
/// The middle page is the only place anything is set up. Sync is one choice,
/// not two: a sync server and the account that belongs to it are made
/// together (ADR 0031), and the card either appears once, finished, or not at
/// all. Without it the app is a local notes app, and says that those notes are
/// not backed up.
library;

import 'package:flutter/material.dart';

import 'setup_widgets.dart';

class OnboardingScreen extends StatefulWidget {
  const OnboardingScreen({
    super.key,
    this.relayConnected = false,
    this.accountCreated = false,
    this.serverUrl,
    required this.onContinue,
    required this.onSetUpSync,
  });

  /// Whether the sync server is connected: the first half of the one setup
  /// step. A connected server always has an account (ADR 0031); the two
  /// fields are kept apart so an interrupted run can say which half is open.
  final bool relayConnected;

  /// Whether this person made an account, which is the second half of setup
  /// and what the send-off page describes.
  final bool accountCreated;

  /// The connected server's address, shown once the first half is done.
  final String? serverUrl;

  final VoidCallback onContinue;

  /// Set up the server and the account that belongs to it, in one flow.
  final VoidCallback onSetUpSync;

  @override
  State<OnboardingScreen> createState() => _OnboardingScreenState();
}

class _OnboardingScreenState extends State<OnboardingScreen> {
  /// Intro, setup, send-off. Named because [_show] takes an index that a
  /// literal in a callback would not explain.
  static const _intro = 0;
  static const _setup = 1;
  static const _done = 2;

  final _pages = PageController();
  int _page = _intro;

  @override
  void didUpdateWidget(OnboardingScreen old) {
    super.didUpdateWidget(old);
    // Finishing the one setup step is the end of the checklist, so it moves on
    // by itself. A server on its own is not a half-finished checklist the
    // person may walk away from; it is a setup that has not finished yet.
    final wasSetUp = old.relayConnected && old.accountCreated;
    final isSetUp = widget.relayConnected && widget.accountCreated;
    if (isSetUp && !wasSetUp) _show(_done);
  }

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  void _show(int page) {
    setState(() => _page = page);
    if (_pages.hasClients) {
      _pages.animateToPage(
        page,
        duration: const Duration(milliseconds: 300),
        curve: Curves.easeOutCubic,
      );
    }
  }

  /// The host of the connected server, which is what a person recognises; the
  /// whole address is not needed to say "connected".
  String? get _host {
    final url = widget.serverUrl;
    if (url == null) return null;
    final host = Uri.tryParse(url)?.host;
    return host == null || host.isEmpty ? url : host;
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    body: SafeArea(
      child: Column(
        children: [
          Expanded(
            child: PageView(
              controller: _pages,
              // Moving on is a choice made with a button, not a swipe: the
              // send-off has nothing to swipe back from, and the checklist
              // should not be skippable by accident.
              physics: const NeverScrollableScrollPhysics(),
              onPageChanged: (page) => setState(() => _page = page),
              children: [_introPage(), _setupPage(), _donePage()],
            ),
          ),
          _PageDots(count: 3, current: _page),
        ],
      ),
    ),
  );

  Widget _introPage() => _PageBody(
    children: [
      Icon(
        Icons.note_alt_outlined,
        size: 56,
        color: Theme.of(context).colorScheme.primary,
      ),
      const SizedBox(height: 24),
      Text(
        'Welcome to EasyNotes',
        key: const Key('onboarding-welcome'),
        textAlign: TextAlign.center,
        style: Theme.of(context).textTheme.headlineSmall,
      ),
      const SizedBox(height: 8),
      const Text(
        'Notes, checklists and reminders. Everything here works offline.',
        textAlign: TextAlign.center,
      ),
      const SizedBox(height: 32),
      const FeatureRow(
        icon: Icons.phone_android,
        title: 'On this device',
        detail:
            'Your notes are saved here. Without sync they are not backed up: '
            'if this device is lost, they are gone.',
      ),
      const SizedBox(height: 20),
      const FeatureRow(
        icon: Icons.cloud_outlined,
        title: 'Sync server & account',
        detail:
            'One setup. Sync your notes between your devices and share them. '
            'The server keeps encrypted copies; your twelve words are what '
            'bring them back on a new device.',
      ),
      const SizedBox(height: 24),
      Text(
        'Sync is optional. You can set it up later in Settings, but an '
        'account needs a sync server and a server needs an account.',
        key: const Key('onboarding-optional'),
        textAlign: TextAlign.center,
        style: Theme.of(context).textTheme.bodySmall,
      ),
      const SizedBox(height: 32),
      SizedBox(
        width: double.infinity,
        child: FilledButton(
          key: const Key('onboarding-next'),
          onPressed: () => _show(_setup),
          child: const Text('Next'),
        ),
      ),
    ],
  );

  Widget _setupPage() {
    final theme = Theme.of(context);
    final setUp = widget.relayConnected && widget.accountCreated;
    final subtitle = setUp
        ? 'Set up with ${_host ?? 'your sync server'}.'
        : widget.relayConnected
        ? 'Connected to ${_host ?? 'your sync server'}. The account step is '
              'still open.'
        : 'Use your notes on other devices and share them. One setup: a '
              'server and the account that belongs to it.';
    return _PageBody(
      children: [
        Text('Set up sync', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        const Text(
          'Sync is optional. Without it your notes stay on this device and '
          'are not backed up.',
        ),
        const SizedBox(height: 24),
        SetupStep(
          key: const Key('onboarding-sync-step'),
          number: 1,
          icon: Icons.cloud_outlined,
          title: 'Sync server & account',
          subtitle: subtitle,
          done: setUp,
          enabled: !setUp,
          onTap: widget.onSetUpSync,
        ),
        const SizedBox(height: 32),
        if (setUp)
          SizedBox(
            width: double.infinity,
            child: FilledButton(
              key: const Key('onboarding-continue'),
              onPressed: () => _show(_done),
              child: const Text('Continue'),
            ),
          )
        else
          SizedBox(
            width: double.infinity,
            child: OutlinedButton(
              key: const Key('onboarding-continue'),
              onPressed: () => _show(_done),
              child: const Text('Continue offline'),
            ),
          ),
      ],
    );
  }

  Widget _donePage() {
    final setUp = widget.relayConnected && widget.accountCreated;
    final body = setUp
        ? 'Your notes sync across your devices, and you can share them with '
              'other people. Your twelve words are what bring them back on a '
              'new one. Tap + to write your first note.'
        : 'Your notes are stored on this device and are not backed up. You '
              'can turn on sync anytime in Settings.';
    return _PageBody(
      children: [
        Icon(
          Icons.check_circle,
          size: 64,
          color: Theme.of(context).colorScheme.primary,
        ),
        const SizedBox(height: 24),
        Text(
          'You’re all set up',
          key: const Key('onboarding-done'),
          textAlign: TextAlign.center,
          style: Theme.of(context).textTheme.headlineSmall,
        ),
        const SizedBox(height: 12),
        Text(body, textAlign: TextAlign.center),
        const SizedBox(height: 32),
        SizedBox(
          width: double.infinity,
          child: FilledButton(
            key: const Key('onboarding-start-writing'),
            onPressed: widget.onContinue,
            child: const Text('Start writing'),
          ),
        ),
      ],
    );
  }
}

/// Three short marks, the current one widened: which of the three pages this
/// is, without numbers a person has to count.
class _PageDots extends StatelessWidget {
  const _PageDots({required this.count, required this.current});

  final int count;
  final int current;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: 24),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          for (var index = 0; index < count; index++)
            AnimatedContainer(
              duration: const Duration(milliseconds: 250),
              curve: Curves.easeOutCubic,
              width: index == current ? 20 : 8,
              height: 8,
              margin: const EdgeInsets.symmetric(horizontal: 4),
              decoration: BoxDecoration(
                color: index == current
                    ? scheme.primary
                    : scheme.outlineVariant,
                borderRadius: BorderRadius.circular(4),
              ),
            ),
        ],
      ),
    );
  }
}

/// The width every page shares, centred, and scrolling when a small screen
/// cannot hold it: the same body the first-run screen always had.
///
/// A column inside one scroll view rather than a `ListView`, so that every
/// line of the page exists even when it is below the fold: nothing here is
/// long enough to be worth building lazily, and an off-screen button is still
/// the button a screen reader or a test should find.
class _PageBody extends StatelessWidget {
  const _PageBody({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) => Center(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 520),
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: children,
        ),
      ),
    ),
  );
}
