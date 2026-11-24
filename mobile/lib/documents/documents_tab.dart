import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;

import '../look.dart';
import '../server.dart';
import '../twins_loading.dart';
import '../screens/controls.dart';
import 'vault_api.dart';
import 'vault_reader.dart';
import 'vault_widgets.dart';

class MobileTabs extends StatefulWidget {
  const MobileTabs({super.key, required this.students, required this.server});

  final Widget students;
  final Server? server;

  @override
  State<MobileTabs> createState() => _MobileTabsState();
}

class _MobileTabsState extends State<MobileTabs> {
  int _selected = 0;
  bool _openedDocuments = false;

  @override
  Widget build(BuildContext context) => Scaffold(
    body: IndexedStack(
      index: _selected,
      children: [
        widget.students,
        if (_openedDocuments)
          DocumentsTab(key: ObjectKey(widget.server), server: widget.server)
        else
          const SizedBox.shrink(),
      ],
    ),
    bottomNavigationBar: NavigationBar(
      height: Look.appBarH,
      selectedIndex: _selected,
      onDestinationSelected: (index) {
        if (index == _selected) return;
        FocusManager.instance.primaryFocus?.unfocus();
        setState(() {
          _selected = index;
          if (index == 1) _openedDocuments = true;
        });
      },
      destinations: const [
        NavigationDestination(
          icon: Icon(Icons.people_outline),
          selectedIcon: Icon(Icons.people),
          label: '학생',
        ),
        NavigationDestination(
          icon: Icon(Icons.folder_copy_outlined),
          selectedIcon: Icon(Icons.folder_copy_rounded),
          label: '문서',
        ),
      ],
    ),
  );
}

class DocumentsTab extends StatefulWidget {
  const DocumentsTab({super.key, required this.server});
  final Server? server;

  @override
  State<DocumentsTab> createState() => _DocumentsTabState();
}

class _DocumentsTabState extends State<DocumentsTab> {
  final _search = TextEditingController();
  final _scroll = ScrollController();
  http.Client? _fixtureClient;
  late final VaultApi? _fixture;
  List<VaultMachine> _machines = const [];
  VaultMachine? _machine;
  VaultApi? _api;
  late Future<VaultDirectory> _listing;
  Future<List<VaultHit>>? _results;
  String _dir = '';
  String _query = '';
  bool _listingChanged = false;
  String? _deviceError;
  bool _loadingDevices = true;

  @override
  void initState() {
    super.initState();
    _fixture = VaultApi.fixture.isEmpty
        ? null
        : VaultApi.debugFixture(_fixtureClient = http.Client());
    if (_fixture case final api?) {
      _api = api;
      _loadingDevices = false;
      _listing = api.list('');
    } else {
      _loadMachines();
    }
  }

  Future<void> _loadMachines() async {
    setState(() {
      _loadingDevices = true;
      _deviceError = null;
    });
    final server = widget.server;
    if (server?.account == null) {
      setState(() {
        _loadingDevices = false;
        _deviceError = server == null
            ? '연결이 돌아오면 문서를 열 수 있어요.'
            : 'KASA 계정으로 연결한 기기의 볼트를 열 수 있어요.';
      });
      return;
    }
    final machines = await server!.vaultMachines();
    if (!mounted) return;
    setState(() {
      _machines = machines;
      _loadingDevices = false;
      if (machines.isEmpty) {
        _deviceError = '계정에 연결된 기기를 찾지 못했어요.';
      } else {
        final preferred = machines.where((m) => m.preferred).toList();
        if (preferred.length == 1) _setMachine(preferred.single);
      }
    });
  }

  void _setMachine(VaultMachine machine) {
    if (_machine?.route == machine.route) return;
    _search.clear();
    _results = null;
    _query = '';
    _dir = '';
    _machine = machine;
    _api = VaultApi.kasanet(widget.server!, machine.route);
    _listingChanged = false;
    _listing = _api!.list('');
    _scrollTop();
  }

  void _scrollTop() {
    if (_scroll.hasClients) _scroll.jumpTo(0);
  }

  void _browse(String dir) {
    _search.clear();
    _scrollTop();
    setState(() {
      _query = '';
      _results = null;
      _dir = dir;
      _listingChanged = false;
      _listing = _api!.list(dir);
    });
  }

  void _find(String value) {
    final query = value.trim();
    if (query.isEmpty) return _clearSearch();
    _scrollTop();
    setState(() {
      _query = query;
      _results = _api!.search(query);
    });
  }

  void _clearSearch() {
    if (_listingChanged) return _browse(_dir);
    _search.clear();
    _scrollTop();
    setState(() {
      _query = '';
      _results = null;
    });
  }

  String get _studyScope => _fixture != null
      ? 'simulator'
      : '${widget.server!.account!.scope}|${_machine!.route}';

  Future<void> _open(String path) async {
    FocusManager.instance.primaryFocus?.unfocus();
    await Navigator.of(context).push<void>(
      MaterialPageRoute(
        builder: (_) => VaultReader(
          api: _api!,
          path: path,
          studyScope: _studyScope,
          onChanged: _refreshListing,
        ),
      ),
    );
  }

  Future<void> _newDocument() async {
    final file = await Navigator.of(context).push<VaultFile>(
      MaterialPageRoute(
        builder: (_) => VaultEditor(api: _api!, directory: _dir),
      ),
    );
    if (!mounted || file == null) return;
    _browse(_dir);
    await _open(file.path);
  }

  void _refreshListing() {
    if (!mounted) return;
    if (_results == null) {
      setState(() {
        _listing = _api!.list(_dir);
      });
    } else {
      _listingChanged = true;
    }
  }

  @override
  void dispose() {
    _fixtureClient?.close();
    _search.dispose();
    _scroll.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final api = _api;
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          title: const Text('문서'),
          actions: [
            if (_machines.isNotEmpty)
              PopupMenuButton<VaultMachine>(
                tooltip: '볼트 기기 선택',
                icon: const Icon(Icons.devices_outlined),
                onSelected: (machine) => setState(() => _setMachine(machine)),
                itemBuilder: (_) => [
                  for (final machine in _machines)
                    CheckedPopupMenuItem(
                      value: machine,
                      checked: machine.route == _machine?.route,
                      child: Text(machine.label),
                    ),
                ],
              ),
            if (api != null)
              IconButton(
                tooltip: '새 문서',
                icon: const Icon(Icons.note_add_outlined),
                onPressed:
                    _results == null &&
                        vaultWritable(_dir.isEmpty ? 'doc.md' : '$_dir/doc.md')
                    ? _newDocument
                    : null,
              ),
          ],
        ),
        body: api == null ? _unconnected() : _library(context),
      ),
    );
  }

  Widget _unconnected() {
    if (_loadingDevices) return const VaultLoading('볼트 기기를 찾는 중');
    if (_deviceError case final error?) {
      return VaultNotice(
        icon: Icons.devices_outlined,
        text: error,
        retry: widget.server?.account == null ? null : _loadMachines,
      );
    }
    return ListView(
      padding: const EdgeInsets.all(Look.pagePad),
      children: [
        const VaultSection('볼트 기기 선택'),
        VaultPanel(
          child: Column(
            children: [
              for (final machine in _machines)
                SettingsRow(
                  icon: Icons.computer_outlined,
                  title: machine.label,
                  chevron: true,
                  onTap: () => setState(() => _setMachine(machine)),
                ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _library(BuildContext context) => Column(
    children: [
      Padding(
        padding: const EdgeInsets.fromLTRB(
          Look.pagePad,
          0,
          Look.pagePad,
          Look.fieldGap,
        ),
        child: TextField(
          controller: _search,
          textInputAction: TextInputAction.search,
          onSubmitted: _find,
          decoration: InputDecoration(
            hintText: '문서를 뜻으로 찾기',
            prefixIcon: IconButton(
              tooltip: '뜻으로 찾기',
              onPressed: () {
                FocusScope.of(context).unfocus();
                _find(_search.text);
              },
              icon: const Icon(Icons.search),
            ),
            suffixIcon: ValueListenableBuilder(
              valueListenable: _search,
              builder: (_, value, _) => value.text.isEmpty
                  ? const SizedBox.shrink()
                  : IconButton(
                      tooltip: '검색 지우기',
                      onPressed: _clearSearch,
                      icon: const Icon(Icons.close),
                    ),
            ),
          ),
        ),
      ),
      Expanded(
        child: switch (_results) {
          final results? => FutureBuilder(
            future: results,
            builder: (context, snapshot) {
              if (snapshot.connectionState != ConnectionState.done) {
                return const VaultLoading('관련 문서를 찾는 중');
              }
              if (snapshot.hasError) {
                return VaultNotice(
                  icon: Icons.search_off,
                  text: vaultError(snapshot.error!),
                  retry: () => _find(_query),
                );
              }
              return _searchResults(snapshot.requireData);
            },
          ),
          null => FutureBuilder(
            key: ValueKey((_machine?.route, _dir)),
            future: _listing,
            builder: (context, snapshot) {
              if (snapshot.hasData) return _directory(snapshot.requireData);
              if (snapshot.connectionState != ConnectionState.done) {
                return const VaultLoading('문서 목록을 받는 중');
              }
              if (snapshot.hasError) {
                return VaultNotice(
                  icon: Icons.folder_off_outlined,
                  text: vaultError(snapshot.error!),
                  retry: () => _browse(_dir),
                );
              }
              return const VaultLoading('문서 목록을 받는 중');
            },
          ),
        },
      ),
    ],
  );

  Widget _directory(VaultDirectory data) => RefreshIndicator(
    onRefresh: () async {
      _refreshListing();
      // FutureBuilder displays any refresh error.
      await _listing.then<void>((_) {}, onError: (Object _) {});
    },
    child: ListView(
      controller: _scroll,
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.fromLTRB(
        Look.pagePad,
        0,
        Look.pagePad,
        Look.groupGap,
      ),
      children: [
        _breadcrumbs(),
        if (data.dirs.isNotEmpty)
          VaultPanel(
            child: Column(
              children: [
                for (final (index, name) in data.dirs.indexed)
                  SettingsRow(
                    icon: name == '_study'
                        ? Icons.style_outlined
                        : Icons.folder_outlined,
                    title: name == '_study' ? '공부 덱' : name,
                    tone: index,
                    chevron: true,
                    onTap: () => _browse(_dir.isEmpty ? name : '$_dir/$name'),
                  ),
              ],
            ),
          ),
        if (data.files.isNotEmpty) ...[
          if (data.dirs.isNotEmpty) const SizedBox(height: Look.groupGap),
          const VaultSection('파일'),
          VaultPanel(
            child: Column(
              children: [
                for (final (index, file) in data.files.indexed)
                  SettingsRow(
                    icon: Icons.description_outlined,
                    title: file.title.isEmpty
                        ? vaultTitle(file.path)
                        : file.title,
                    subtitle: file.path.split('/').last,
                    tone: index,
                    chevron: true,
                    onTap: () => _open(file.path),
                  ),
              ],
            ),
          ),
        ],
        if (data.dirs.isEmpty && data.files.isEmpty)
          const VaultNotice(icon: Icons.folder_open, text: '이 폴더에 문서가 없어요.'),
      ],
    ),
  );

  Widget _breadcrumbs() {
    final dirs = _dir.isEmpty ? const <String>[] : _dir.split('/');
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Row(
        children: [
          TextButton.icon(
            onPressed: dirs.isEmpty ? null : () => _browse(''),
            icon: const Icon(Icons.folder_open, size: Look.iconSize),
            label: Text(_machine?.label ?? '메모리'),
          ),
          for (final (index, dir) in dirs.indexed) ...[
            const Icon(Icons.chevron_right, size: Look.iconSize),
            TextButton(
              onPressed: index == dirs.length - 1
                  ? null
                  : () => _browse(dirs.take(index + 1).join('/')),
              child: Text(dir == '_study' ? '공부 덱' : dir),
            ),
          ],
        ],
      ),
    );
  }

  Widget _searchResults(List<VaultHit> hits) {
    if (hits.isEmpty) {
      return const VaultNotice(icon: Icons.search_off, text: '관련 문서를 찾지 못했어요.');
    }
    final theme = Theme.of(context);
    return ListView.builder(
      controller: _scroll,
      padding: const EdgeInsets.fromLTRB(
        Look.pagePad,
        0,
        Look.pagePad,
        Look.groupGap,
      ),
      itemCount: hits.length + 1,
      itemBuilder: (context, index) {
        if (index == 0) return VaultSection('검색 결과 ${hits.length}개');
        final hit = hits[index - 1];
        return Padding(
          padding: const EdgeInsets.only(bottom: Look.cardGap),
          child: VaultPanel(
            child: SettingsRow(
              icon: Icons.description_outlined,
              title: vaultTitle(hit.path),
              subtitle: hit.path,
              tone: index - 1,
              chevron: true,
              onTap: () => _open(hit.path),
              below: hit.snippets.isEmpty
                  ? null
                  : Padding(
                      padding: const EdgeInsets.fromLTRB(
                        Look.cardPad,
                        0,
                        Look.cardPad,
                        Look.cardPad,
                      ),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          for (final snippet in hit.snippets.take(2)) ...[
                            const SizedBox(height: Look.groupTitleGap),
                            if (snippet.head.isNotEmpty)
                              Text(
                                snippet.head,
                                style: theme.textTheme.labelLarge,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            Text(
                              snippet.text,
                              maxLines: 3,
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.bodyMedium,
                            ),
                          ],
                        ],
                      ),
                    ),
            ),
          ),
        );
      },
    );
  }
}
