import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'links.dart';
import 'server.dart';

String? resultFilePath(String href, String cwd) {
  if (href.trim().isEmpty || href.startsWith('#') || href.startsWith('?')) {
    return null;
  }
  final target = href.replaceFirst(RegExp(r':\d+(?::\d+)?$'), '');
  final uri = Uri.tryParse(target);
  if (uri == null ||
      ['http', 'https', 'mailto'].contains(uri.scheme.toLowerCase())) {
    return null;
  }
  if (uri.scheme == 'file') {
    if (uri.host.isNotEmpty && uri.host != 'localhost') return null;
  } else if (uri.hasScheme || uri.hasAuthority) {
    return null;
  }
  final String path;
  try {
    path = uri.replace(fragment: '', query: '').toFilePath();
  } catch (_) {
    return null;
  }
  if (path.contains('\u0000') || path.isEmpty) return null;
  if (path.startsWith('/') || path.startsWith('~/')) return path;
  if (cwd.isEmpty || !cwd.startsWith('/')) return null;
  return Uri.directory(
    cwd,
  ).resolveUri(uri.replace(fragment: '', query: '')).toFilePath();
}

Future<void> showResultLink(
  BuildContext context,
  Server server,
  String href, {
  required String cwd,
  String? machine,
}) {
  final path = resultFilePath(href, cwd);
  return showLinkSheet(
    context,
    href,
    onOpenFile: path == null
        ? null
        : () => previewResultFile(context, server, path, machine: machine),
  );
}

/// 원본 기기의 파일을 받아 폰 미리보기로 연다 — 링크 시트의 「파일 열기」와 빈 셸 트리의 파일.
Future<void> previewResultFile(
  BuildContext context,
  Server server,
  String path, {
  String? machine,
}) async {
  final messenger = ScaffoldMessenger.of(context);
  messenger.showSnackBar(
    const SnackBar(content: Text('파일을 가져오는 중이에요')),
  );
  try {
    final bytes = await server.resultFileBytes(path, machine: machine);
    if (!context.mounted) return;
    final name = path.split('/').last;
    await const MethodChannel(
      'kasaterm/file_preview',
    ).invokeMethod<void>('preview', {'name': name, 'bytes': bytes});
  } catch (error) {
    if (context.mounted) {
      messenger.hideCurrentSnackBar();
      messenger.showSnackBar(
        SnackBar(
          content: Text(
            error is ServerException ? error.message : '이 파일의 미리보기를 열지 못했어요',
          ),
        ),
      );
    }
    return;
  }
  if (context.mounted) messenger.hideCurrentSnackBar();
}
