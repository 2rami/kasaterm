import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:kasaterm_mobile/conversation_cache.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/term_session.dart';
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_repository.dart';
import 'package:kasaterm_mobile/server.dart';

import 'archive_fixtures.dart';

class Source extends Server {
  Source() : super(Uri.parse('https://source.invalid/'), client: MockClient((_) async => throw StateError('unexpected network')));
  String raw = message('user', 'first'), source = 'one.jsonl';
  final offsets = <int>[];
  bool online = true;
  @override
  Future<Map<String, dynamic>> conversationArchive(Pane pane, int offset, String held) async {
    if (!online) throw const ServerException('offline');
    offsets.add(offset);
    final changed = held.isNotEmpty && held != source;
    final bytes = utf8.encode(raw);
    return {'ok':true,'archive_version':1,'address':{'machine_id':'machine-a','surface_key':'key','surface_id':'%1'},
      'source':source,'reset':changed || offset > bytes.length,
      'offset':bytes.length,'raw':utf8.decode(bytes.sublist(changed || offset > bytes.length ? 0 : offset)),
      'machine_name':'fixture'};
  }
}
const pane = Pane(id:'%1', name:'Assistant', title:'conversation', status:'working', window:0,
    cwd:'/fixture', surfaceKey:'key', harness:'claude');

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  testWidgets('cached messages appear before a slow source and a changed source replaces them', (tester) async {
    final live=Completer<http.Response>();
    final server=Server(Uri.parse('https://source.invalid/'),client:MockClient((req) async {
      if (req.url.path.endsWith('transcript-raw')) return live.future;
      return http.Response('{}',404);
    }));
    final store=MemoryArchiveStore();
    final raw=message('assistant','cached answer');
    await store.save(server.resumeScope,SavedConversation(entry:entryFor(raw,key:'key'),raw:raw,savedAt:DateTime.utc(2026)),current:()=>true);
    final cache=ConversationCache(server,repository:ArchiveRepository(server,store:store));
    ConversationCache.bind(server,cache);
    final session=TermSession(server,pane);
    await tester.pumpWidget(MaterialApp(home:Scaffold(body:ConversationView(
      server:server,pane:pane,session:session,accent:Colors.blue,onTerminal:(){},
    ))));
    for (var i=0;i<10;i++) { await tester.pump(const Duration(milliseconds:10)); }
    expect(find.textContaining('cached answer',findRichText:true),findsOneWidget);
    expect(find.text('저장된 대화 · 연결 확인 중'),findsOneWidget);
    expect(find.text('오프라인 저장'),findsNothing);
    live.complete(http.Response(jsonEncode({'ok':true,'raw':message('assistant','fresh different session'),
      'offset':5000,'source':'new.jsonl','reset':true}),200,headers:{'content-type':'application/json'}));
    for (var i=0;i<10;i++) { await tester.pump(const Duration(milliseconds:10)); }
    expect(find.textContaining('cached answer',findRichText:true),findsNothing);
    expect(find.textContaining('fresh different session',findRichText:true),findsOneWidget);
    expect(find.text('저장된 대화 · 연결 확인 중'),findsNothing);
    await tester.pumpWidget(const SizedBox());
    session.dispose();cache.dispose();server.close();
  });

  test('background suspension cancels incomplete bytes without pausing future automatic saves', () async {
    final server=Server.account(archiveSession('background'),connectDirect:false);
    final raw=message('assistant','resume automatic download');
    final api=FakeArchiveApi(entryFor(raw),raw)..readGate=Completer<void>();
    final repo=ArchiveRepository(server,store:MemoryArchiveStore(),api:api);
    final pending=repo.save(api.entry);
    await api.started.future;
    repo.cancel(pause:false);
    api.readGate!.complete();
    await expectLater(pending,throwsA(isA<ArchiveException>()));
    expect(await repo.autoAllowed(api.entry.id),isTrue);
    api.readGate=null;
    await repo.refreshRecent();
    expect((await repo.open(api.entry.id))?.raw,raw);
    repo.dispose();server.close();
  });

  test('new account records download without an explicit save and do not redownload unchanged data', () async {
    final server = Server.account(archiveSession(), connectDirect:false);
    final raw = message('assistant','automatic');
    final api = FakeArchiveApi(entryFor(raw),raw);
    final store = MemoryArchiveStore();
    final repo = ArchiveRepository(server,store:store,api:api);
    await repo.refreshRecent();
    expect((await repo.open(api.entry.id))?.raw,raw);
    expect(api.requested,[0]);
    await repo.refreshRecent();
    expect(api.requested,[0]);
    await repo.remove(api.entry.id);
    await repo.refreshRecent();
    expect(await repo.open(api.entry.id),isNull);
    repo.dispose(); server.close();
  });

  test('automatic source cache resumes by original cursor and opens offline after repository recreation', () async {
    final server=Source();
    final store=MemoryArchiveStore();
    var repo=ArchiveRepository(server,store:store);
    final first=await repo.savePane(pane,direct:true);
    final offset=utf8.encode(server.raw).length;
    expect(server.offsets,[0,offset]);
    server.offsets.clear();
    server.raw+=message('assistant','second');
    final all=await repo.savePane(pane,direct:true);
    expect(server.offsets.first,offset);
    expect(all.raw,server.raw);
    expect(all.raw.startsWith(first.raw),isTrue);
    repo.dispose();
    server.online=false;
    repo=ArchiveRepository(server,store:store);
    expect((await repo.openPane(pane))?.raw,server.raw);
    final reused=Pane(id:'%1',name:'other',title:'other',status:'working',window:0,cwd:'/fixture',surfaceKey:'other');
    expect(await repo.openPane(reused),isNull);
    repo.dispose();server.close();
  });

  test('a new transcript resets the cache and a deleted record is not automatically resurrected', () async {
    final server=Source();
    final repo=ArchiveRepository(server,store:MemoryArchiveStore());
    await repo.savePane(pane,direct:true);
    server.source='two.jsonl';server.raw=message('assistant','different session');
    final next=await repo.savePane(pane,direct:true);
    expect(next.raw,server.raw);
    expect(next.entry.source,'two.jsonl');
    await repo.remove(next.entry.id);
    await expectLater(repo.savePane(pane,source:'two.jsonl',direct:true),throwsA(isA<ArchiveException>()));
    expect(await repo.open(next.entry.id),isNull);
    repo.dispose();server.close();
  });
}
