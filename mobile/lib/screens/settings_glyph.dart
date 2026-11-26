import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';

enum SettingsGlyph { appearance, weather, account, agents, characters, devices, release }

class SettingsMark extends StatelessWidget {
  const SettingsMark(this.glyph, {super.key, this.size = 20, this.color});

  final SettingsGlyph glyph;
  final double size;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final ink = color ?? Theme.of(context).colorScheme.primary;
    return ExcludeSemantics(
      child: SvgPicture.asset(
        'assets/icons/settings/${glyph.name}.svg',
        width: size,
        height: size,
        colorFilter: ColorFilter.mode(ink, BlendMode.srcIn),
      ),
    );
  }
}
