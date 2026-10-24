/// The pieces first-run setup and Settings share: a setup card that fills and
/// ticks itself, and one line of "what this is" with an icon.
///
/// Sync is one decision in both places — a server and the account that belongs
/// to it — and telling it the same way twice is the point: a person who saw
/// the card on the first run recognises it in Settings.
library;

import 'package:flutter/material.dart';

/// One line of "what this app is", with an icon and a sentence of detail.
class FeatureRow extends StatelessWidget {
  const FeatureRow({
    super.key,
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
class SetupStep extends StatelessWidget {
  const SetupStep({
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
