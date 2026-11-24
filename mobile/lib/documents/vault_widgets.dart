import 'package:flutter/material.dart';

import '../look.dart';
import '../twins_loading.dart';

class VaultPanel extends StatelessWidget {
  const VaultPanel({super.key, required this.child});
  final Widget child;

  @override
  Widget build(BuildContext context) => Material(
    color: TwinTone.of(context).card,
    shape: RoundedRectangleBorder(borderRadius: Look.cardCorners),
    clipBehavior: Clip.antiAlias,
    child: child,
  );
}

class VaultSection extends StatelessWidget {
  const VaultSection(this.text, {super.key});
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(
      bottom: Look.groupTitleGap,
      left: Look.groupTitleGap,
    ),
    child: Text(text, style: Theme.of(context).textTheme.titleSmall),
  );
}

class VaultLoading extends StatelessWidget {
  const VaultLoading(this.text, {super.key});
  final String text;

  @override
  Widget build(BuildContext context) => Center(
    child: TwinsLoading(label: text, size: Look.twinsSmall),
  );
}

class VaultNotice extends StatelessWidget {
  const VaultNotice({
    super.key,
    required this.icon,
    required this.text,
    this.retry,
  });
  final IconData icon;
  final String text;
  final VoidCallback? retry;

  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(Look.groupGap),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, color: Theme.of(context).colorScheme.onSurfaceVariant),
          const SizedBox(height: Look.groupTitleGap),
          Text(text, textAlign: TextAlign.center),
          if (retry != null) ...[
            const SizedBox(height: Look.groupTitleGap),
            FilledButton.tonal(onPressed: retry, child: const Text('다시 시도')),
          ],
        ],
      ),
    ),
  );
}
