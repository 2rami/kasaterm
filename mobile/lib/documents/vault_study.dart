import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:fsrs/fsrs.dart' as fsrs;
import 'package:markdown/markdown.dart' as md;
import 'package:yaml/yaml.dart';

typedef StudyCard = ({String front, String back});

class VaultMarkdown {
  VaultMarkdown(String source) {
    final header = _header.firstMatch(source);
    final match = header?.start == 0 ? header : null;
    var content = match == null ? source : source.substring(match.end);
    if (match != null) {
      try {
        final yaml = loadYaml(match[1]!);
        if (yaml is Map && yaml['deck'] == true) {
          _front = yaml['front']?.toString();
          _back = yaml['back']?.toString();
        }
      } on YamlException {
        content = source;
      }
    }
    body = content;
  }

  static final _header = RegExp(
    r'^\uFEFF?---[ \t]*\r?\n([\s\S]*?)^---[ \t]*(?:\r?\n|$)',
    multiLine: true,
  );

  late final String body;
  String? _front;
  String? _back;

  late final _tables = _elements(
    md.Document(extensionSet: md.ExtensionSet.gitHubFlavored).parse(body),
    'table',
  ).toList();

  bool get hasWideTable =>
      _tables.any((table) => _elements([table], 'th').length > 3);

  late final List<StudyCard> cards = _readCards();

  List<StudyCard> _readCards() {
    final byFront = <String, StudyCard>{};
    for (final table in _tables) {
      final headers = _elements([
        table,
      ], 'th').map((e) => e.textContent.trim()).toList();
      if (headers.length < 2) continue;
      final front = _front == null ? 0 : headers.indexOf(_front!);
      final back = _back == null ? null : headers.indexOf(_back!);
      if (front < 0 || back == -1 || back == front) continue;
      for (final row in _elements([table], 'tr')) {
        final cells = _elements([
          row,
        ], 'td').map((e) => e.textContent.trim()).toList();
        if (cells.length != headers.length) continue;
        final prompt = cells[front];
        final answer = back == null
            ? [
                for (var i = 0; i < cells.length; i++)
                  if (i != front && cells[i].isNotEmpty) cells[i],
              ].join('\n')
            : cells[back];
        if (prompt.isNotEmpty && answer.isNotEmpty) {
          byFront.putIfAbsent(prompt, () => (front: prompt, back: answer));
        }
      }
    }
    return byFront.values.toList(growable: false);
  }

  static Iterable<md.Element> _elements(
    Iterable<md.Node> nodes,
    String tag,
  ) sync* {
    for (final node in nodes.whereType<md.Element>()) {
      if (node.tag == tag) {
        yield node;
      } else if (node.children case final children?) {
        yield* _elements(children, tag);
      }
    }
  }
}

class VaultStudyStore {
  const VaultStudyStore(this.scope);
  final String scope;
  static const _storage = FlutterSecureStorage(
    iOptions: IOSOptions(
      accessibility: KeychainAccessibility.unlocked_this_device,
    ),
  );

  String _key(String deck, String front) =>
      'vault.study.v1.${sha256.convert(utf8.encode(jsonEncode([scope, deck, front])))}';

  Future<fsrs.Card?> read(String deck, StudyCard card) async {
    final saved = await _storage.read(key: _key(deck, card.front));
    return saved == null
        ? null
        : fsrs.Card.fromMap(jsonDecode(saved) as Map<String, dynamic>);
  }

  Future<void> write(String deck, StudyCard card, fsrs.Card state) => _storage
      .write(key: _key(deck, card.front), value: jsonEncode(state.toMap()));
}
