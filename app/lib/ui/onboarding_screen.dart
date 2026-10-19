import 'package:flutter/material.dart';

/// First-run choices for keeping notes here or connecting a relay.
class OnboardingScreen extends StatelessWidget {
  const OnboardingScreen({
    super.key,
    this.relayConnected = false,
    required this.onContinue,
    required this.onSetUpRelay,
    required this.onCreateAccount,
  });

  final bool relayConnected;
  final VoidCallback onContinue;
  final VoidCallback onSetUpRelay;
  final VoidCallback onCreateAccount;

  @override
  Widget build(BuildContext context) => Scaffold(
    body: SafeArea(
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: ListView(
            shrinkWrap: true,
            padding: const EdgeInsets.all(24),
            children: [
              Icon(
                Icons.note_alt_outlined,
                size: 56,
                color: Theme.of(context).colorScheme.primary,
              ),
              const SizedBox(height: 24),
              Text(
                'Your notes are saved on this device.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.headlineSmall,
              ),
              const SizedBox(height: 8),
              Text(
                'A relay and account are optional.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.titleMedium,
              ),
              const SizedBox(height: 32),
              Text(
                relayConnected
                    ? 'Your relay can store encrypted backups without an account.'
                    : 'Want backups? Connect a relay. You can use it without '
                          'an account.',
                key: const Key('onboarding-relay-copy'),
              ),
              const SizedBox(height: 16),
              const Text(
                'Want device sync and sharing? Create an account and use it '
                'with your relay.',
                key: Key('onboarding-account-copy'),
              ),
              const SizedBox(height: 32),
              if (relayConnected) ...[
                FilledButton.icon(
                  key: const Key('onboarding-create-account'),
                  onPressed: onCreateAccount,
                  icon: const Icon(Icons.key),
                  label: const Text('Create account'),
                ),
                const SizedBox(height: 12),
                OutlinedButton(
                  key: const Key('onboarding-continue-relay-only'),
                  onPressed: onContinue,
                  child: const Text('Continue with relay only'),
                ),
              ] else ...[
                FilledButton(
                  key: const Key('onboarding-continue-local'),
                  onPressed: onContinue,
                  child: const Text('Continue on this device'),
                ),
                const SizedBox(height: 12),
                OutlinedButton.icon(
                  key: const Key('onboarding-set-up-relay'),
                  onPressed: onSetUpRelay,
                  icon: const Icon(Icons.cloud_outlined),
                  label: const Text('Set up a relay'),
                ),
              ],
            ],
          ),
        ),
      ),
    ),
  );
}
