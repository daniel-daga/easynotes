/// What the app says when it could not keep an edit.
///
/// The counterpart to [SyncButton], and the more serious of the two. That one
/// answers "is what I typed anywhere but here?", and a no is temporary — the
/// edit is in the log and goes when there is a network. This answers "is what I
/// typed even *here*?", and a no is not temporary: the core handed the write
/// over once and kept no copy, so the edit is gone and no amount of waiting
/// brings it back.
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
/// had gone away, and it has not — the next edit is about as likely to be
/// refused as the last one was, and the one that was already refused is not
/// coming back. It goes when the tab does.
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
                  // What happened, and what it means, in that order. "Not
                  // saved" alone would leave someone waiting for it to retry.
                  'This browser would not keep an edit, and it is gone. '
                  'Copy anything you need before closing the tab.',
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
            'The browser refused to store an edit. It was not kept anywhere '
            'else, so it cannot be retried. Notes that were already stored are '
            'still there.',
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
