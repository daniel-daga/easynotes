/// What this app sends where, who to ask about it, and whose code it is.
///
/// The privacy notice a store asks for has to be reachable from inside the app,
/// and it has to be true of the build that shows it. So the part that describes
/// data flow is written here, beside the code it describes, and the parts that
/// depend on who publishes the app — the policy's address and a support
/// contact — are given to the build rather than written into the source:
///
/// ```sh
/// flutter build appbundle \
///   --dart-define=EASYNOTES_VERSION=1.2.3 \
///   --dart-define=EASYNOTES_PRIVACY_URL=https://example.org/privacy \
///   --dart-define=EASYNOTES_SUPPORT=support@example.org
/// ```
///
/// The release workflow refuses to build without the last two.
library;

import 'package:flutter/foundation.dart'
    show LicenseEntryWithLineBreaks, LicenseRegistry;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/relay_profiles.dart';

/// The release this build was made from, or empty for a development build.
const appVersion = String.fromEnvironment('EASYNOTES_VERSION');

/// Where the published privacy policy is, or empty when the build was not
/// told — which only a development build should be.
const privacyPolicyUrl = String.fromEnvironment('EASYNOTES_PRIVACY_URL');

/// An address or URL a person can reach the publisher at, for support,
/// security reports and data-protection requests.
const supportContact = String.fromEnvironment('EASYNOTES_SUPPORT');

/// Where this build's source is. The AGPL obliges whoever conveys the app to
/// offer it, so a fork that publishes its own build points this at its own.
const sourceUrl = String.fromEnvironment(
  'EASYNOTES_SOURCE_URL',
  defaultValue: 'https://github.com/daniel-daga/easynotes',
);

/// The Roboto the app bundles is Apache-2.0 and is not a package, so the
/// licence page would not list it unless told to.
void registerBundledLicenses() {
  LicenseRegistry.addLicense(() async* {
    yield LicenseEntryWithLineBreaks(const [
      'Roboto',
    ], await rootBundle.loadString('assets/fonts/LICENSE.txt'));
  });
}

class AboutScreen extends StatelessWidget {
  const AboutScreen({super.key, required this.profile});

  /// The profile in use, because what leaves the device depends on it.
  final RelayProfile profile;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final relay = profile.url;
    Widget heading(String text) => Padding(
      padding: const EdgeInsets.only(top: 24, bottom: 8),
      child: Text(text, style: theme.textTheme.titleMedium),
    );
    Widget para(String text) =>
        Padding(padding: const EdgeInsets.only(bottom: 8), child: Text(text));

    return Scaffold(
      appBar: AppBar(title: const Text('About and privacy')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text('EasyNotes', style: theme.textTheme.headlineSmall),
          Text(
            appVersion.isEmpty ? 'Development build' : 'Version $appVersion',
            key: const Key('about-version'),
          ),
          heading('What leaves this device'),
          if (relay == null)
            para(
              'Nothing. This profile keeps its notes on this device only and '
              'connects to no server. They are not backed up anywhere: if this '
              'device is lost or the app is removed, they are gone.',
            )
          else ...[
            para(
              'Your notes sync through the relay at $relay. Whoever runs that '
              'relay operates it, not the app.',
            ),
            para(
              'Every note is encrypted on this device before it is sent. The '
              'relay stores and forwards the encrypted copies and cannot read '
              'titles, text, labels, colours or reminders.',
            ),
            para(
              'It does see what any server sees: your IP address, a random key '
              'that identifies this device (not you), which encrypted notes it '
              'reads and writes, when, how large they are, and the invitations '
              'used to share them. It keeps the encrypted notes, and its '
              'operator may keep backups of them.',
            ),
          ],
          para(
            'The app has no analytics, advertising or crash reporting, and '
            'reminders are scheduled on this device only.',
          ),
          para(
            'Deleting a note for good removes it from this device, and from '
            'your other devices on the same account when they next sync. It '
            'does not remove the copy '
            'held by someone you shared it with, or encrypted copies already '
            'in a relay\'s history or backups.',
          ),
          para(
            'Deleting your account, from Your devices, removes every list you '
            'made from the relay, including ones you shared, and everything '
            'from this device. Lists others shared with you stay theirs. A '
            'relay\'s backups may keep encrypted copies for up to 30 days.',
          ),
          heading('Privacy policy and support'),
          if (privacyPolicyUrl.isEmpty && supportContact.isEmpty)
            para(
              'This build was not published with a privacy policy or support '
              'contact.',
            ),
          if (privacyPolicyUrl.isNotEmpty)
            _Copyable(
              key: const Key('privacy-policy'),
              label: 'Privacy policy',
              value: privacyPolicyUrl,
            ),
          if (supportContact.isNotEmpty)
            _Copyable(
              key: const Key('support-contact'),
              label: 'Support, security and data requests',
              value: supportContact,
            ),
          const Divider(height: 32),
          para(
            'EasyNotes is free software under the GNU Affero General Public '
            'License, version 3 or later. You may study, change and share it; '
            'it comes with no warranty.',
          ),
          _Copyable(
            key: const Key('source-url'),
            label: 'Source code',
            value: sourceUrl,
          ),
          ListTile(
            key: const Key('licenses-entry'),
            contentPadding: EdgeInsets.zero,
            leading: const Icon(Icons.description_outlined),
            title: const Text('Open-source licences'),
            onTap: () => showLicensePage(
              context: context,
              applicationName: 'EasyNotes',
              applicationVersion: appVersion.isEmpty ? null : appVersion,
              applicationLegalese: 'AGPL-3.0-or-later. Source: $sourceUrl',
            ),
          ),
        ],
      ),
    );
  }
}

/// A link the app cannot open, because it has no browser plugin and does not
/// need one for this: shown whole, selectable, and one tap from the clipboard.
class _Copyable extends StatelessWidget {
  const _Copyable({super.key, required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) => ListTile(
    contentPadding: EdgeInsets.zero,
    title: Text(label),
    subtitle: SelectableText(value),
    trailing: IconButton(
      tooltip: 'Copy',
      icon: const Icon(Icons.copy),
      onPressed: () async {
        await Clipboard.setData(ClipboardData(text: value));
        if (!context.mounted) return;
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('$label copied')));
      },
    ),
  );
}
