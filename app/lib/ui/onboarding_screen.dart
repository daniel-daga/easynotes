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
                relayConnected ? 'Create account' : 'Set-up cloud backups.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.headlineSmall,
              ),
              const SizedBox(height: 32),
              Text(
                relayConnected
                    ? 'Your notes will be synced to the cloud instances!'
                    : 'Connect to a cloud instance to enable online backups.',
                key: const Key('onboarding-relay-copy'),
              ),
              const SizedBox(height: 16),
              Text(
                relayConnected
                    ? 'Create an account if you want to sync across devices and share notes. You can always set this up later.' : 'Once connected to an instance, you can also create an account - enabling sync between your devices and note sharing.',
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
                  child: const Text('Continue without account'),
                ),
              ] else ...[
                FilledButton(
                  key: const Key('onboarding-continue-local'),
                  onPressed: onContinue,
                  child: const Text('Continue offline'),
                ),
                const SizedBox(height: 12),
                OutlinedButton.icon(
                  key: const Key('onboarding-set-up-relay'),
                  onPressed: onSetUpRelay,
                  icon: const Icon(Icons.cloud_outlined),
                  label: const Text('Set-up cloud backups'),
                ),
              ],
            ],
          ),
        ),
      ),
    ),
  );
}
