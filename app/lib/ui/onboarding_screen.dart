/// The first run: what the app is, how its two optional levels of sync work,
/// those two steps as a checklist, and a send-off.
///
/// The middle page is the only place anything is set up. Each step is a card
/// that becomes a filled, ticked one when it is done, so that a person sees
/// both steps and their own progress rather than one screen that changes its
/// mind underneath them. The account step stays disabled until a sync server
/// is connected, because an account has nowhere to live without one.
library;

import 'package:flutter/material.dart';

class OnboardingScreen extends StatefulWidget {
  const OnboardingScreen({
    super.key,
    this.relayConnected = false,
    this.accountCreated = false,
    this.serverUrl,
    required this.onContinue,
    required this.onSetUpRelay,
    required this.onCreateAccount,
  });

  /// Whether a sync server is connected: the first step is done, and the
  /// second one is open.
  final bool relayConnected;

  /// Whether this person made an account, which is the last thing onboarding
  /// asks for and what the send-off page describes.
  final bool accountCreated;

  /// The connected server's address, shown once the first step is done.
  final String? serverUrl;

  final VoidCallback onContinue;
  final VoidCallback onSetUpRelay;
  final VoidCallback onCreateAccount;

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
    // Making the account is the end of the checklist, so finishing it moves
    // on by itself; a server alone leaves the deliberate second choice
    // ("continue without account") in the person's hands.
    if (widget.accountCreated && !old.accountCreated) _show(_done);
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
      const _FeatureRow(
        icon: Icons.phone_android,
        title: 'On this device',
        detail:
            'Your notes are saved here, and nobody needs an account '
            'to write them.',
      ),
      const SizedBox(height: 20),
      const _FeatureRow(
        icon: Icons.cloud_outlined,
        title: 'Sync server',
        detail:
            'Backs up your notes online. Use one from your organization '
            'or a sync server provider.',
      ),
      const SizedBox(height: 20),
      const _FeatureRow(
        icon: Icons.key,
        title: 'Account',
        detail:
            'Syncs your notes between your devices and lets you share '
            'them. It needs a sync server.',
      ),
      const SizedBox(height: 24),
      Text(
        'Both steps are optional. You can set them up later in Settings.',
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
    final serverDone = widget.relayConnected;
    final accountDone = widget.accountCreated;
    final accountOpen = serverDone && !accountDone;
    return _PageBody(
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: IconButton(
            key: const Key('onboarding-back'),
            tooltip: 'Back',
            icon: const Icon(Icons.arrow_back),
            onPressed: () => _show(_intro),
            padding: EdgeInsets.zero,
            alignment: Alignment.centerLeft,
          ),
        ),
        Text('Set up sync', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        const Text(
          'Both steps are optional. You can change them later in Settings.',
        ),
        const SizedBox(height: 24),
        _SetupStep(
          key: const Key('onboarding-server-step'),
          number: 1,
          icon: Icons.cloud_outlined,
          title: 'Sync server',
          subtitle: serverDone
              ? 'Connected to ${_host ?? 'your sync server'}.'
              : 'Back up your notes online.',
          done: serverDone,
          enabled: !serverDone,
          onTap: widget.onSetUpRelay,
        ),
        _Connector(
          color: serverDone
              ? theme.colorScheme.primary
              : theme.colorScheme.outlineVariant,
        ),
        _SetupStep(
          key: const Key('onboarding-account-step'),
          number: 2,
          icon: Icons.key,
          title: 'Account',
          subtitle: accountDone
              ? 'Account created.'
              : serverDone
              ? 'Sync between your devices and share notes.'
              : 'Connect a sync server first.',
          done: accountDone,
          enabled: accountOpen,
          onTap: widget.onCreateAccount,
        ),
        const SizedBox(height: 32),
        if (accountOpen)
          SizedBox(
            width: double.infinity,
            child: OutlinedButton(
              key: const Key('onboarding-continue'),
              onPressed: () => _show(_done),
              child: const Text('Continue without account'),
            ),
          )
        else if (!serverDone)
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
    final host = _host;
    final body = widget.accountCreated
        ? 'Your notes sync across your devices, and you can share them with '
              'other people. Tap + to write your first note.'
        : widget.relayConnected
        ? 'Your notes are backed up to ${host ?? 'your sync server'}. Create '
              'an account anytime in Settings to sync between your devices '
              'and share notes.'
        : 'Your notes are stored on this device. You can turn on sync '
              'anytime in Settings.';
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

/// One line of "what this app is", with an icon and a sentence of detail.
class _FeatureRow extends StatelessWidget {
  const _FeatureRow({
    required this.icon,
    required this.title,
    required this.detail,
  });

  final IconData icon;
  final String title;
  final String detail;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 40,
          child: Icon(icon, color: theme.colorScheme.onSurfaceVariant),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                title,
                style: theme.textTheme.titleMedium?.copyWith(
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 2),
              Text(
                detail,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A checklist step: outlined and tappable, greyed out and not, or filled and
/// ticked once it is done.
///
/// Done is deliberately a different shape from available — a filled card with
/// a tick instead of a number and no chevron — so that success is visible
/// without comparing two shades of the same colour.
class _SetupStep extends StatelessWidget {
  const _SetupStep({
    super.key,
    required this.number,
    required this.icon,
    required this.title,
    required this.subtitle,
    required this.done,
    required this.enabled,
    this.onTap,
  });

  final int number;
  final IconData icon;
  final String title;
  final String subtitle;
  final bool done;
  final bool enabled;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dim = !enabled && !done;
    final content = done || enabled
        ? scheme.onSurface
        : scheme.onSurface.withValues(alpha: 0.45);
    final detail = done || enabled
        ? scheme.onSurfaceVariant
        : scheme.onSurface.withValues(alpha: 0.4);

    return Semantics(
      button: enabled && !done,
      enabled: enabled && !done,
      label: done ? '$title, done' : title,
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 300),
        curve: Curves.easeOutCubic,
        decoration: BoxDecoration(
          color: done ? scheme.primaryContainer : scheme.surface,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(
            color: done
                ? Colors.transparent
                : dim
                ? scheme.outlineVariant.withValues(alpha: 0.5)
                : scheme.outlineVariant,
          ),
        ),
        child: Material(
          type: MaterialType.transparency,
          child: InkWell(
            borderRadius: BorderRadius.circular(16),
            onTap: enabled && !done ? onTap : null,
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Row(
                children: [
                  AnimatedContainer(
                    duration: const Duration(milliseconds: 300),
                    curve: Curves.easeOutCubic,
                    width: 36,
                    height: 36,
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      color: done
                          ? scheme.primary
                          : scheme.surfaceContainerHighest,
                      border: done
                          ? null
                          : Border.all(color: scheme.outlineVariant),
                    ),
                    child: Center(
                      child: AnimatedSwitcher(
                        duration: const Duration(milliseconds: 250),
                        transitionBuilder: (child, animation) =>
                            ScaleTransition(scale: animation, child: child),
                        child: done
                            ? Icon(
                                Icons.check,
                                key: const ValueKey('done'),
                                size: 20,
                                color: scheme.onPrimary,
                              )
                            : Text(
                                '$number',
                                key: const ValueKey('number'),
                                style: theme.textTheme.titleMedium?.copyWith(
                                  color: dim ? detail : scheme.onSurface,
                                ),
                              ),
                      ),
                    ),
                  ),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          children: [
                            Icon(icon, size: 18, color: detail),
                            const SizedBox(width: 6),
                            Expanded(
                              child: Text(
                                title,
                                style: theme.textTheme.titleMedium?.copyWith(
                                  color: content,
                                  fontWeight: FontWeight.w600,
                                ),
                              ),
                            ),
                          ],
                        ),
                        const SizedBox(height: 2),
                        AnimatedSwitcher(
                          duration: const Duration(milliseconds: 250),
                          child: Text(
                            subtitle,
                            key: ValueKey(subtitle),
                            style: theme.textTheme.bodyMedium?.copyWith(
                              color: detail,
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                  if (enabled && !done) ...[
                    const SizedBox(width: 8),
                    Icon(Icons.chevron_right, color: scheme.onSurfaceVariant),
                  ],
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// The line that joins the two steps, filled once the first is done.
///
/// Its left edge sits under the number badges above and below: the page's
/// padding, the card's padding, half a badge, and half a line.
class _Connector extends StatelessWidget {
  const _Connector({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(left: 24 + 16 + 17, top: 2, bottom: 2),
    child: Row(
      children: [
        AnimatedContainer(
          duration: const Duration(milliseconds: 300),
          curve: Curves.easeOutCubic,
          width: 2,
          height: 18,
          color: color,
        ),
      ],
    ),
  );
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
