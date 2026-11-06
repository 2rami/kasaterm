import 'dart:async';

import 'package:flutter/material.dart';

import '../look.dart';
import '../server.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'desktop_controls.dart';

class DesktopSettingsScreen extends StatefulWidget {
  const DesktopSettingsScreen({
    super.key,
    required this.server,
    required this.title,
    this.machine,
  });
  final Server server;
  final String title;
  final String? machine;
  @override
  State<DesktopSettingsScreen> createState() => _DesktopSettingsScreenState();
}

class _DesktopSettingsScreenState extends State<DesktopSettingsScreen> {
  Map<String, dynamic>? _values;
  String? _error;
  bool _busy = false;
  Timer? _refresh;
  String _filter = '';

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _refresh?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final values = await widget.server.desktopValues(
        'settings/values',
        machine: widget.machine,
      );
      if (mounted) {
        setState(() {
          _values = values;
          _error = null;
        });
        _refresh?.cancel();
        if (desktopMap(desktopMap(values['lid_away'])['hotspot'])['busy'] == true) {
          _refresh = Timer(const Duration(seconds: 2), _load);
        }
      }
    } catch (error) {
      if (mounted) setState(() => _error = desktopError(error));
    }
  }

  Future<void> _apply(String action, {String? id, String? label}) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.server.settingsAction(
        action,
        id: id,
        label: label,
        machine: widget.machine,
      );
      await _load();
    } catch (error) {
      if (mounted) setState(() => _error = desktopError(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Widget _choice(
    String title,
    Object? current,
    String action,
    List<(String, String)> choices,
  ) {
    final key = '${current ?? ''}';
    final label = choices.where((c) => c.$1 == key).firstOrNull?.$2 ?? key;
    return SettingsRow(
      title: title,
      subtitle: label,
      icon: Icons.tune,
      onTap: _busy
          ? null
          : () async {
              final next = await desktopChoice(
                context,
                title,
                choices,
                selected: key,
              );
              if (next != null && mounted) await _apply(action, id: next);
            },
    );
  }

  Widget _text(
    String title,
    Object? current,
    String action, {
    bool empty = false,
  }) => SettingsRow(
    title: title,
    subtitle: '${current ?? ''}',
    icon: Icons.edit_outlined,
    onTap: _busy
        ? null
        : () async {
            final value = await desktopText(
              context,
              title,
              '${current ?? ''}',
              allowEmpty: empty,
            );
            if (value != null && mounted) await _apply(action, label: value);
          },
  );

  Widget _toggle(
    String title,
    bool value,
    String action, {
    String? id,
    bool explicit = false,
  }) => SwitchListTile.adaptive(
    title: Text(title),
    value: value,
    onChanged: _busy
        ? null
        : (next) => _apply(action, id: id, label: explicit ? '$next' : null),
  );

  Widget _step(String title, Object? value, String action) => ListTile(
    title: Text(title),
    subtitle: Text('${value ?? ''}'),
    trailing: Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        IconButton(
          tooltip: '$title 줄이기',
          onPressed: _busy ? null : () => _apply(action, id: '-1'),
          icon: const Icon(Icons.remove),
        ),
        IconButton(
          tooltip: '$title 늘리기',
          onPressed: _busy ? null : () => _apply(action, id: '1'),
          icon: const Icon(Icons.add),
        ),
      ],
    ),
  );

  @override
  Widget build(BuildContext context) {
    final v = _values;
    final g = desktopMap(v?['general']);
    final a = desktopMap(v?['appearance']);
    final c = desktopMap(v?['claude']);
    final agents = desktopMap(v?['agents']);
    final lid = desktopMap(v?['lid_away']);
    final hotspot = desktopMap(lid['hotspot']);
    final themes = [
      for (final t in desktopRows(a['themes']))
        ('${t['key']}', '${t['label']}'),
    ];
    final groups = <String, List<Widget>>{
      if (lid['supported'] == true)
        '덮개·핫스팟': [
          _toggle('덮어도 마저 하고 잠자기', lid['on'] == true,
              'lid-away-enabled', explicit: true),
          if (lid['on'] == true)
            _choice('덮인 뒤 최대', lid['minutes'], 'lid-away-minutes',
                const [('10', '10분'), ('30', '30분'), ('60', '60분')]),
          if (lid['on'] == true && lid['ready'] != true)
            const ListTile(title: Text('맥에서 잠자기 권한 설정이 필요해요'),
                subtitle: Text('맥 설정의 연결 기기 → 덮어도 마저 하기 안내를 확인해 주세요')),
          _toggle('덮으면 폰 핫스팟 연결', lid['hotspot_on'] == true,
              'lid-away-hotspot', explicit: true),
          _text('등록할 핫스팟 이름', lid['hotspot_ssid'],
              'lid-away-hotspot-ssid', empty: true),
          SettingsRow(title: '지금 연결 확인', icon: Icons.wifi,
              subtitle: hotspot['busy'] == true ? '연결 확인 중' : '${hotspot['status'] ?? ''}',
              onTap: _busy || hotspot['busy'] == true || '${lid['hotspot_ssid'] ?? ''}'.isEmpty
                  ? null : () => _apply('lid-away-hotspot-test')),
          const ListTile(subtitle: Text('마저 하고 잠자기가 켜져 있고 작업 중일 때 연결합니다. 먼저 맥 Wi-Fi 메뉴에서 폰에 한 번 연결해 주세요. iPhone 자동 활성화는 macOS의 핫스팟 자동 연결을 사용합니다.')),
          if ('${lid['status'] ?? ''}'.isNotEmpty)
            ListTile(subtitle: Text('${lid['status']}')),
        ],
      '에이전트': [
        if (agents.containsKey('preferred_agent'))
          _choice(
            '기본 에이전트',
            agents['preferred_agent'],
            'preferred-agent',
            const [('claude', 'Claude'), ('codex', 'Codex')],
          ),
        for (final provider in ['claude', 'codex'])
          if (desktopMap(agents['permissions']).containsKey(provider))
            SettingsRow(
              title: '${provider == 'claude' ? 'Claude' : 'Codex'} 실행 권한',
              subtitle: switch (desktopMap(agents['permissions'])[provider]) {
                'workspace' => '작업 폴더 허용',
                'unrestricted' => '제한 없이 실행',
                _ => '기본 확인',
              },
              icon: Icons.shield_outlined,
              onTap: _busy
                  ? null
                  : () async {
                      final mode = await desktopChoice(
                        context,
                        '${provider == 'claude' ? 'Claude' : 'Codex'} 실행 권한',
                        const [
                          ('default', '기본 확인'),
                          ('workspace', '작업 폴더 허용'),
                          ('unrestricted', '제한 없이 실행'),
                        ],
                        selected:
                            desktopMap(agents['permissions'])[provider]
                                as String?,
                      );
                      if (mode == null || !context.mounted) return;
                      if (mode == 'unrestricted' &&
                          !await desktopConfirm(
                            context,
                            '이 기기에서 새로 시작하는 $provider 작업이 제한 없이 실행돼요. 적용할까요?',
                            action: '적용',
                          )) {
                        return;
                      }
                      if (mounted) {
                        await _apply(
                          'agent-permission',
                          id: provider,
                          label: mode,
                        );
                      }
                    },
            ),
        if (c.containsKey('model'))
          _choice('Claude 모델', c['model'], 'claude-model', const [
            ('', '기본값'),
            ('opus', 'Opus'),
            ('sonnet', 'Sonnet'),
            ('haiku', 'Haiku'),
          ]),
        if (c.containsKey('effort'))
          _choice('Claude 추론 깊이', c['effort'], 'claude-effort', const [
            ('', '기본값'),
            ('low', '낮음'),
            ('medium', '보통'),
            ('high', '높음'),
            ('xhigh', '최대'),
          ]),
        if (c.containsKey('extra'))
          _text('Claude 추가 실행 옵션', c['extra'], 'claude-extra', empty: true),
        if (c.containsKey('shim_inject'))
          _toggle(
            '에이전트 연결 도우미',
            c['shim_inject'] == true,
            'toggle-shim-inject',
          ),
        if (c.containsKey('autoswitch'))
          _toggle(
            '한도 도달 시 계정 자동 전환',
            c['autoswitch'] == true,
            'toggle-account-autoswitch',
          ),
        if (c.containsKey('autoswitch_pct'))
          _choice('계정 전환 사용량', c['autoswitch_pct'], 'autoswitch-pct', const [
            ('80', '80%'),
            ('85', '85%'),
            ('90', '90%'),
            ('95', '95%'),
          ]),
        if (desktopMap(agents['statusline']).containsKey('customized'))
          _toggle(
            '에이전트 상태줄 직접 설정',
            desktopMap(agents['statusline'])['customized'] == true,
            'agent-statusline-custom',
            explicit: true,
          ),
        for (final (field, name) in [
          ('model', '모델 표시'),
          ('usage', '사용량 표시'),
          ('cwd', '작업 폴더 표시'),
        ])
          if (desktopMap(agents['statusline'])['customized'] == true)
            _toggle(
              name,
              desktopMap(agents['statusline'])[field] == true,
              'agent-statusline',
              id: field,
              explicit: true,
            ),
      ],
      '외형': [
        if (themes.isNotEmpty)
          _choice('PC 색 테마', a['theme'], 'theme-mode', themes),
        if (a.containsKey('theme_system_light'))
          _choice(
            '밝은 모드 테마',
            a['theme_system_light'],
            'theme-system-light',
            themes.where((e) => e.$1 != 'system').toList(),
          ),
        if (a.containsKey('theme_system_dark'))
          _choice(
            '어두운 모드 테마',
            a['theme_system_dark'],
            'theme-system-dark',
            themes.where((e) => e.$1 != 'system').toList(),
          ),
        if (a.containsKey('accents'))
          _choice('강조색', a['accent'], 'accent', [
            for (final t in desktopRows(a['accents']))
              ('${t['name']}', '${t['name']}'),
          ]),
        if (a.containsKey('shapes'))
          _choice('모서리', a['shape'], 'shape', [
            for (final t in desktopRows(a['shapes']))
              ('${t['key']}', '${t['label']}'),
          ]),
        if (a.containsKey('contrasts'))
          _choice(
            '최소 대비',
            desktopRows(a['contrasts'])
                .where((e) => e['value'] == a['min_contrast'])
                .firstOrNull?['label'],
            'min-contrast',
            [
              for (final t in desktopRows(a['contrasts']))
                ('${t['label']}', '${t['label']}'),
            ],
          ),
        if (a.containsKey('font_size'))
          _step('PC 터미널 글자 크기', a['font_size'], 'font-size-delta'),
        if (a.containsKey('ui_zoom'))
          _step(
            'PC 화면 배율',
            '${((a['ui_zoom'] as num) * 100).round()}%',
            'ui-zoom-delta',
          ),
        if (a.containsKey('ui_zoom'))
          SettingsRow(
            title: '글자·배율 기본값',
            icon: Icons.restart_alt,
            onTap: _busy ? null : () => _apply('reset-scale'),
          ),
      ],
      '셸·파일': [
        if (g.containsKey('cwd_mode'))
          _choice('새 칸의 시작 폴더', g['cwd_mode'], 'cwd-mode', const [
            ('last', '마지막 폴더'),
            ('home', '홈 폴더'),
          ]),
        if (g.containsKey('cwd_mode'))
          _text(
            '시작 폴더 직접 지정',
            g['cwd_mode'] == 'home' || g['cwd_mode'] == 'last'
                ? ''
                : g['cwd_mode'],
            'cwd-path',
          ),
        if (v?.containsKey('shell') == true)
          _text('기본 셸 경로', desktopMap(v!['shell'])['shell'], 'shell-custom'),
        if (g.containsKey('file_tree_default'))
          _toggle(
            '파일트리 기본으로 열기',
            g['file_tree_default'] == true,
            'toggle-file-tree',
          ),
        if (g.containsKey('file_open_mode'))
          _choice('PC 파일 열기', g['file_open_mode'], 'file-open-mode', const [
            ('builtin', '카사텀 편집기'),
            ('app', '외부 앱'),
            ('terminal', '터미널 명령'),
          ]),
        if (g['file_open_mode'] == 'app')
          _choice('파일을 여는 앱', g['file_open_app'], 'file-open-app', [
            ('', '기본 앱'),
            for (final app in desktopRows(g['apps']))
              ('${app['name']}', '${app['short']}'),
          ]),
        if (g['file_open_mode'] == 'terminal')
          _text('파일 열기 명령', g['file_open_cmd'], 'file-open-cmd'),
        if (g.containsKey('autosave_ms'))
          _choice('편집기 자동 저장', g['autosave_ms'], 'autosave-delay', const [
            ('0', '끔'),
            ('1000', '1초'),
            ('3000', '3초'),
            ('10000', '10초'),
          ]),
      ],
      'PC 배치·입력': [
        if (g.containsKey('tabs_on_top'))
          _choice(
            '방 탭 위치',
            g['tabs_on_top'] == true ? 'top' : 'side',
            'tab-position',
            const [('top', '위'), ('side', '옆')],
          ),
        if (g.containsKey('pane_header'))
          _choice('칸 머리', g['pane_header'], 'pane-header', const [
            ('bar', '항상 표시'),
            ('handle', '작은 메뉴'),
          ]),
        if (g.containsKey('cursor_shape'))
          _choice('터미널 커서', g['cursor_shape'], 'cursor-shape', const [
            ('block', '블록'),
            ('bar', '세로줄'),
            ('underline', '밑줄'),
            ('frame', '테두리'),
            ('brackets', '괄호'),
            ('twin-rails', '양쪽 세로줄'),
            ('topline', '윗줄'),
            ('corner-marks', '모서리'),
          ]),
        if (g.containsKey('cursor_thickness'))
          _choice(
            '커서 굵기',
            (g['cursor_thickness'] as num?)?.round(),
            'cursor-thickness',
            const [('1', '1px'), ('2', '2px'), ('3', '3px'), ('4', '4px')],
          ),
        if (g.containsKey('mouse_cursor'))
          _choice('마우스 포인터', g['mouse_cursor'], 'mouse-cursor', const [
            ('arrow', '화살표'),
            ('ibeam', '글자 선택'),
          ]),
        if (g.containsKey('wheel_gain_x100'))
          _choice('스크롤 감도', g['wheel_gain_x100'], 'wheel-gain', const [
            ('30', '0.3배'),
            ('60', '0.6배'),
            ('100', '1배'),
            ('150', '1.5배'),
          ]),
      ],
    };
    String rowTitle(Widget row) => switch (row) {
      SettingsRow(:final title) => title,
      SwitchListTile(title: Text(:final data)) => data ?? '',
      ListTile(title: Text(:final data)) => data ?? '',
      _ => '',
    };
    final filtered = <String, List<Widget>>{
      for (final group in groups.entries)
        group.key: group.key.toLowerCase().contains(_filter.toLowerCase())
            ? group.value
            : group.value
                  .where(
                    (row) => rowTitle(
                      row,
                    ).toLowerCase().contains(_filter.toLowerCase()),
                  )
                  .toList(),
    };
    return Scaffold(
      appBar: AppBar(title: Text('${widget.title} 설정')),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          padding: const EdgeInsets.all(Look.pagePad),
          children: [
            if (_busy) const LinearProgressIndicator(),
            if (_error != null) ErrorBand(text: _error!, onRetry: _load),
            if (v == null && _error == null)
              const TwinsLoading(label: 'PC 설정을 받는 중', size: Look.twinsSmall),
            if (v != null) ...[
              TextField(
                decoration: const InputDecoration(
                  hintText: '설정 검색',
                  prefixIcon: Icon(Icons.search),
                ),
                onChanged: (value) => setState(() => _filter = value.trim()),
              ),
              const SizedBox(height: Look.groupGap),
              for (final group in filtered.entries)
                if (group.value.isNotEmpty)
                  SettingsGroup(title: group.key, children: group.value),
            ],
          ],
        ),
      ),
    );
  }
}
