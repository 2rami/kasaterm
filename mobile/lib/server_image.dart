import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'server.dart';

class ServerImage extends StatefulWidget {
  const ServerImage({
    super.key,
    required this.server,
    required this.uri,
    this.width,
    this.height,
    this.fit,
    this.semanticLabel,
    this.alignment = Alignment.center,
    this.fallback = const SizedBox.shrink(),
  });
  final Server server;
  final Uri uri;
  final double? width;
  final double? height;
  final BoxFit? fit;
  final Alignment alignment;
  final String? semanticLabel;
  final Widget fallback;

  @override
  State<ServerImage> createState() => _ServerImageState();
}

class _ServerImageState extends State<ServerImage> {
  late Future<Uint8List> _bytes = widget.server.imageBytes(widget.uri);

  @override
  void didUpdateWidget(ServerImage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.server != widget.server || oldWidget.uri != widget.uri) {
      _bytes = widget.server.imageBytes(widget.uri);
    }
  }

  @override
  Widget build(BuildContext context) => FutureBuilder<Uint8List>(
    future: _bytes,
    builder: (context, snapshot) {
      if (widget.server.isClosed ||
          snapshot.connectionState != ConnectionState.done ||
          !snapshot.hasData) {
        return widget.fallback;
      }
      return Image.memory(
        snapshot.data!,
        width: widget.width,
        height: widget.height,
        fit: widget.fit,
        alignment: widget.alignment,
        semanticLabel: widget.semanticLabel,
        errorBuilder: (_, _, _) => widget.fallback,
      );
    },
  );
}
