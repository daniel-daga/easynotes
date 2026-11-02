/// What the app says when it cannot keep what is typed.
///
/// Sync status lives in the home drawer. A failure to keep an edit is more
/// serious: sync answers "is what I typed anywhere but here?", and a no is
/// temporary — the edit is in the log and goes when there is a network. This
/// answers "is what I typed even *here*?", and a no is not temporary: either
/// the core handed the write over once and kept no copy, or the browser never
/// gave the app a store to write to. Either way no amount of waiting brings it
/// back.
///
/// It can only happen in a browser, for the reason [StorageHealth] gives. It is
/// drawn on both screens because a person is as likely to be typing into a list
/// as looking at the grid, and a warning they have to go back to see is a
/// warning they see too late.
library;

import 'package:flutter/material.dart';

import '../core/note_library.dart';

class StorageBanner extends StatelessWidget {
  const StorageBanner({super.key, required this.storage});

  final StorageHealth storage;

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<String?>(
      stream: storage.watchStorageError(),
      initialData: storage.storageError,
      builder: (context, snapshot) {
        final error = snapshot.data;
        // Nothing at all rather than an empty bar: on every platform but the
        // web this is the answer for ever, and a strip of padding above every
        // note list would be the cost of a message that cannot arrive.
        if (error == null) return const SizedBox.shrink();
        return _Bar(error: error);
      },
    );
  }
}

/// The bar itself: one sentence, in the theme's error colours, that opens the
/// browser's own words when tapped.
///
/// No dismiss button. Dismissing it would be the app agreeing that the problem
/// had gone away, and it has not — the next edit will be no better kept than
/// the last one, and anything already lost is not coming back. It goes when the
/// tab does.
class _Bar extends StatelessWidget {
  const _Bar({required this.error});

  final String error;

  @override
  Widget build(BuildContext context) {
    final colours = Theme.of(context).colorScheme;
    return Material(
      color: colours.errorContainer,
      child: InkWell(
        key: const Key('storage-error'),
        onTap: () => _showDetail(context, error),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
          child: Row(
            children: [
              Icon(
                Icons.warning_amber_outlined,
                color: colours.onErrorContainer,
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  // What is true here, in both ways this banner arrives: a
                  // write the browser refused, and a browser that gave the tab
                  // no store at all. "An edit is gone" would be a lie in the
                  // second, where no edit was ever handed over.
                  'This tab is not keeping what you type. '
                  'Copy anything you need before closing it.',
                  style: TextStyle(color: colours.onErrorContainer),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The browser's own message, for whoever can do something with it.
///
/// Kept off the bar for the reason [syncSentence] keeps `ECONNREFUSED` out of a
/// tooltip: "QuotaExceededError" is not a sentence anyone can act on, and the
/// one above it is.
Future<void> _showDetail(BuildContext context, String error) {
  return showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Not saved'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Text(
            'Nothing typed in this tab is being kept, so it cannot be retried '
            'and will not survive a reload. Anything that was stored before is '
            'untouched.',
          ),
          const SizedBox(height: 12),
          SelectableText(
            error,
            key: const Key('storage-error-detail'),
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Done'),
        ),
      ],
    ),
  );
}
