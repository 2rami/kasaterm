use super::*;
use serde_json::{json, Value};

fn matches_name(row: &crate::render::pane_identity::DeviceNameRow, machine: &MachineRow) -> bool {
    if let Some(id) = machine.machine_id.as_deref() {
        id.trim_start_matches('~') == row.id.trim_start_matches('~')
    } else {
        let normalized = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        normalized(&row.label) == normalized(&machine.label)
    }
}

pub(super) fn fixture(s: &mut Snapshot) {
    use crate::render::pane_identity::DeviceNameRow;
    s.device_names = Arc::new(vec![
        DeviceNameRow {
            id: "fixture-local".into(),
            label: "검증 MacBook".into(),
            name: Some("현재 기기".into()),
            local: true,
        },
        DeviceNameRow {
            id: "fixture-mini".into(),
            label: "작업용 미니".into(),
            name: Some("작업 서버".into()),
            local: false,
        },
        DeviceNameRow {
            id: "fixture-windows".into(),
            label: "Windows PC".into(),
            name: None,
            local: false,
        },
    ]);
    for row in Arc::make_mut(&mut s.machines) {
        if row.machine_id.as_deref() == Some("fixture-mini") {
            row.online = true;
            row.build_match = true;
            row.status = "연결됨".into();
        }
    }
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    section_title(g, x, *y, "이 기기", "");
    *y += 54.0;
    if let Some(local) = s.device_names.iter().find(|row| row.local) {
        device_row(g, s, hits, caret, x, y, w, Some(local), None);
    } else {
        let name = crate::info::cached_local_machine_name().unwrap_or("이 기기");
        flat_row(g, x, *y, w, name, "현재 설정을 바꾸는 기기", w);
        *y += ROW_H;
    }
    plain_hint(
        g,
        x,
        y,
        w,
        "표시 이름은 같은 KASA 계정의 PC·폰에서 함께 사용합니다. 연결 주소는 바뀌지 않습니다.",
    );
    *y += 8.0;
    section_title(g, x, *y, "다른 기기", "");
    button(
        g,
        s,
        hits,
        (x + w - 92.0, *y + 22.0, 92.0, CTL_H),
        "기기 추가",
        Target::Setting(SettingsAction::AddMachine),
        false,
    );
    *y += 54.0;
    if let Some(editor) = s.machine_edit.as_ref() {
        paint_editor(g, s, hits, caret, x, y, w, editor);
    }
    let mut count = 0;
    for (index, machine) in s.machines.iter().enumerate() {
        let name = s.device_names.iter().find(|row| matches_name(row, machine));
        if name.is_some_and(|row| row.local) {
            continue;
        }
        device_row(g, s, hits, caret, x, y, w, name, Some((index, machine)));
        count += 1;
    }
    for name in s.device_names.iter().filter(|row| !row.local) {
        if s.machines.iter().any(|machine| matches_name(name, machine)) {
            continue;
        }
        device_row(g, s, hits, caret, x, y, w, Some(name), None);
        count += 1;
    }
    if count == 0 {
        plain_hint(
            g,
            x,
            y,
            w,
            "연결된 다른 기기가 없습니다. 다른 기기의 앱에 같은 KASA 계정으로 로그인해 주세요.",
        );
        plain_hint(
            g,
            x,
            y,
            w,
            "직접 연결할 기기는 기기 추가에서 SSH 별칭이나 주소를 등록할 수 있습니다.",
        );
    }
    *y += 12.0;
    if !disclosure(
        g,
        s,
        hits,
        x,
        y,
        w,
        "device-tools",
        "브라우저·자동 전환 설정",
    ) {
        return;
    }
    section_title(g, x, *y, "브라우저 도구", "");
    *y += 54.0;
    let names: Vec<_> = s
        .chrome_candidates
        .iter()
        .map(|label| crate::render::pane_identity::device_name(label))
        .collect();
    let mut cells = vec![(
        "이 기기",
        s.chrome_machine.is_empty(),
        SettingsAction::ChromeMachine(String::new()),
    )];
    for (label, name) in s.chrome_candidates.iter().zip(&names) {
        cells.push((
            name.as_str(),
            s.chrome_machine == *label,
            SettingsAction::ChromeMachine(label.clone()),
        ));
    }
    seg_row(g, s, hits, x, y, w, "크롬이 있는 기기", &cells);
    plain_hint(
        g,
        x,
        y,
        w,
        if s.chrome_machine.is_empty() {
            "브라우저 도구는 이 기기의 Chrome을 사용합니다."
        } else {
            "선택한 기기의 Chrome을 사용합니다. 연결되지 않으면 이 기기의 Chrome을 사용합니다."
        },
    );
    #[cfg(target_os = "macos")]
    paint_lid_away(g, s, hits, x, y, w);
}

fn device_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    name: Option<&crate::render::pane_identity::DeviceNameRow>,
    machine: Option<(usize, &MachineRow)>,
) {
    let label = name
        .map(|row| row.label.as_str())
        .or_else(|| machine.map(|(_, row)| row.label.as_str()))
        .unwrap_or("");
    let shown = name
        .and_then(|row| row.name.as_deref())
        .filter(|name| !name.is_empty())
        .unwrap_or(if label.is_empty() {
            "이름 없는 연결"
        } else {
            label
        });
    g.queue_icon(
        &crate::device_icons::icon(label),
        x,
        *y + 8.0,
        theme::ICON_SIZE,
        theme::text_dim(),
    );
    let tx = x + theme::ICON_SIZE + 12.0;
    if let Some((_, value)) = s
        .device_name_edit
        .as_ref()
        .filter(|(id, _)| name.is_some_and(|row| row.id == *id))
    {
        text_field(
            g,
            s,
            hits,
            caret,
            tx,
            *y + 4.0,
            w - (tx - x),
            "",
            value,
            SettingsInput::DeviceName,
            s.settings_caret,
            false,
        );
        let hint = fit(
            g,
            "Enter 완료 · Esc 취소 · 비우면 원래 이름 사용",
            (w - (tx - x)).max(0.0),
            10.5,
            false,
        );
        draw_text(g, tx, *y + 36.0, &hint, 10.5, theme::text_dim(), false);
    } else {
        let editable = machine.filter(|(_, row)| !row.guest);
        let mut right = x + w;
        if let Some((index, _)) = editable {
            right -= CTL_H;
            mini_icon_button(
                g,
                s,
                hits,
                (right, *y + 6.0, CTL_H, CTL_H),
                "trash-2",
                Target::Setting(SettingsAction::RemoveMachine(index)),
            );
            right -= CTL_H + 8.0;
            mini_icon_button(
                g,
                s,
                hits,
                (right, *y + 6.0, CTL_H, CTL_H),
                "settings-2",
                Target::Setting(SettingsAction::FocusMachineField(index, true)),
            );
        }
        if let Some(row) = name {
            right -= CTL_H + 8.0;
            mini_icon_button(
                g,
                s,
                hits,
                (right, *y + 6.0, CTL_H, CTL_H),
                "pencil",
                Target::Setting(SettingsAction::FocusDeviceName(row.id.clone())),
            );
        }
        let shown = crate::info::fit_text(g, shown, (right - tx - 8.0).max(0.0), 12.0, false);
        g.draw_text(
            tx,
            *y + 6.0,
            &shown,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        let status = if name.is_some_and(|row| row.local) {
            "사용 중 · 이 기기"
        } else {
            machine
                .map(|(_, row)| row.status.as_str())
                .unwrap_or("연결 상태 확인 필요")
        };
        let status = fit(g, status, (w - (tx - x)).max(0.0), 10.5, false);
        draw_text(g, tx, *y + 24.0, &status, 10.5, theme::text_dim(), false);
        if let Some((_, machine)) = machine {
            let address = display_address(&machine.ssh);
            let detail = if machine.key.is_empty() {
                address
            } else {
                format!("{address} · 키 {}", machine.key)
            };
            let detail = fit(g, &detail, (w - (tx - x)).max(0.0), 10.5, false);
            draw_text(g, tx, *y + 41.0, &detail, 10.5, theme::text_dim(), false);
        }
    }
    *y += 60.0;
    if let Some((_, machine)) = machine.filter(|(_, row)| {
        row.online && !row.build_match && !row.guest && !row.sync_target.is_empty()
    }) {
        mini_text_button(
            g,
            s,
            hits,
            tx,
            *y,
            "rotate-cw",
            "새 판 보내기",
            Target::Setting(SettingsAction::SyncMachine(
                machine.label.clone(),
                machine.sync_target.clone(),
            )),
            false,
        );
        *y += ROW_H;
    }
    g.rect(x, *y - 1.0, w, 1.0, theme::with_alpha(theme::border(), 140));
}

fn display_address(value: &str) -> String {
    if let Some(word) = value.split_whitespace().next() {
        if let Ok(mut url) = url::Url::parse(word) {
            if matches!(url.scheme(), "http" | "https") {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                return url.to_string();
            }
        }
    }
    value.to_string()
}

fn paint_editor(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
    editor: &Editor,
) {
    let title = if editor.deleting {
        "이 기기 연결을 제거할까요?"
    } else if editor.original.is_some() {
        "연결 정보 수정"
    } else {
        "새 기기 연결"
    };
    let error_ink = theme::enforce_contrast_at(theme::danger(), theme::bg(), 4.5);
    draw_text(
        g,
        x,
        *y,
        title,
        12.0,
        if editor.deleting {
            error_ink
        } else {
            theme::text()
        },
        true,
    );
    *y += 24.0;
    if editor.deleting {
        let target = crate::info::fit_text(g, &editor.label, w, 12.0, false);
        g.draw_text(
            x,
            *y,
            &target,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        *y += 24.0;
        plain_hint(
            g,
            x,
            y,
            w,
            "목록에서 연결만 제거하며 기기의 앱·세션은 유지합니다",
        );
    } else {
        for (label, value, field) in [
            ("연결 이름", &editor.label, SettingsInput::MachineLabel),
            ("연결 주소", &editor.address, SettingsInput::MachineAddress),
        ] {
            if w < 420.0 {
                draw_text(g, x, *y, label, 12.0, theme::text_dim(), false);
                *y += 20.0;
                text_field(
                    g,
                    s,
                    hits,
                    caret,
                    x,
                    *y,
                    w,
                    "",
                    value,
                    field,
                    s.settings_caret,
                    false,
                );
                *y += CTL_H + 8.0;
            } else {
                text_field(
                    g,
                    s,
                    hits,
                    caret,
                    x,
                    *y,
                    w,
                    label,
                    value,
                    field,
                    s.settings_caret,
                    false,
                );
                *y += ROW_H;
            }
        }
        plain_hint(
            g,
            x,
            y,
            w,
            "저장하기 전에는 연결 목록이 바뀌지 않습니다. Tab으로 다음 입력칸으로 이동합니다.",
        );
    }
    if let Some(error) = editor.error.as_deref() {
        for line in wrap_words(g, error, w, 10.5) {
            draw_text(g, x, *y, &line, 10.5, error_ink, false);
            *y += 18.0;
        }
    }
    *y += 8.0;
    button(
        g,
        s,
        hits,
        (x + w - 164.0, *y, 64.0, CTL_H),
        "취소",
        Target::Setting(SettingsAction::CancelMachine),
        false,
    );
    let action_label = crate::native_strings::text(if editor.deleting {
        "연결 제거"
    } else {
        "저장"
    });
    let rect = crate::native_controls::content_button(
        g,
        (x + w - 92.0, *y, 92.0, CTL_H),
        s.cursor,
        &action_label,
        crate::native_controls::Style {
            danger: editor.deleting,
            primary: !editor.deleting,
            ..Default::default()
        },
    );
    register_clipped(
        g,
        hits,
        Target::Setting(SettingsAction::SaveMachine),
        rect,
        HitCursor::Pointer,
    );
    *y += ROW_H + 12.0;
}

#[derive(Clone, Default)]
pub(crate) struct Editor {
    pub(crate) original: Option<(usize, Value)>,
    pub(crate) label: String,
    pub(crate) address: String,
    pub(crate) deleting: bool,
    pub(crate) error: Option<String>,
}

impl Editor {
    pub(crate) fn existing(index: usize, entry: &Value) -> Self {
        let text = |key| entry.get(key).and_then(Value::as_str).unwrap_or_default();
        let address = ["base", "ssh", "host"]
            .into_iter()
            .map(text)
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        Self {
            original: Some((index, entry.clone())),
            label: text("label").into(),
            address: address.into(),
            ..Default::default()
        }
    }

    pub(crate) fn value(&self, address: bool) -> &str {
        if address {
            &self.address
        } else {
            &self.label
        }
    }

    pub(crate) fn value_mut(&mut self, address: bool) -> &mut String {
        self.error = None;
        if address {
            &mut self.address
        } else {
            &mut self.label
        }
    }

    pub(crate) fn updated(&self, entries: &[Value]) -> Result<Vec<Value>, String> {
        if let Some((index, original)) = &self.original {
            if entries.get(*index) != Some(original) {
                return Err(
                    "기기 목록이 바뀌었습니다. 취소 후 해당 기기를 다시 선택해 주세요".into(),
                );
            }
        }
        let mut result = entries.to_vec();
        if self.deleting {
            let (index, _) = self.original.as_ref().ok_or("삭제할 기기가 없습니다")?;
            result.remove(*index);
            return Ok(result);
        }
        let label = self.label.trim();
        if label.is_empty() {
            return Err("연결 이름을 입력해 주세요".into());
        }
        if label.chars().count() > 64 || label.chars().any(char::is_control) {
            return Err("연결 이름은 줄바꿈 없이 64자까지 입력해 주세요".into());
        }
        use unicode_normalization::UnicodeNormalization;
        let normalized = |s: &str| {
            s.nfc()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        if entries.iter().enumerate().any(|(index, entry)| {
            self.original.as_ref().is_none_or(|(old, _)| index != *old)
                && entry["label"]
                    .as_str()
                    .is_some_and(|s| normalized(s) == normalized(label))
        }) {
            return Err("같은 연결 이름이 있습니다. 구분할 수 있는 이름을 입력해 주세요".into());
        }

        let address = self.address.trim();
        if address.is_empty() {
            return Err("연결 주소를 입력해 주세요".into());
        }
        if address.starts_with('-') || address.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err("연결 주소에 공백·줄바꿈·명령 옵션을 넣을 수 없습니다".into());
        }
        let http = address.to_ascii_lowercase().starts_with("http://")
            || address.to_ascii_lowercase().starts_with("https://");
        let target = if http {
            let parsed =
                url::Url::parse(address).map_err(|_| "올바른 http(s) 주소를 입력해 주세요")?;
            if parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(
                    "주소에는 기기 호스트·포트·경로만 입력해 주세요. 로그인 정보는 넣지 않습니다"
                        .into(),
                );
            }
            parsed.to_string()
        } else if address.contains("://")
            || address.contains([';', '|', '&', '$', '`', '\'', '"', '<', '>'])
        {
            return Err("SSH 별칭·user@host 또는 http(s) 주소를 입력해 주세요".into());
        } else {
            address.to_string()
        };
        let mut entry = self
            .original
            .as_ref()
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| json!({}));
        let object = entry.as_object_mut().ok_or("기기 항목을 읽지 못했습니다")?;
        object.insert("label".into(), json!(label));
        object.insert(if http { "base" } else { "ssh" }.into(), json!(target));
        // An explicit HTTP route can retain SSH metadata for browser forwarding.
        let was_http = self
            .original
            .as_ref()
            .and_then(|(_, value)| value["base"].as_str())
            .is_some_and(|s| !s.is_empty());
        if !http {
            object.remove("base");
        } else if !was_http {
            object.remove("ssh");
        }
        object.remove("account_sync_unresolved");
        if let Some((index, _)) = &self.original {
            result[*index] = entry;
        } else {
            result.push(entry);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_device_is_only_a_draft_until_both_fields_are_valid() {
        let entries = vec![json!({"label":"작업 서버", "ssh":"old"})];
        let mut editor = Editor::default();
        assert!(editor.updated(&entries).is_err());
        let mut korean = editor.clone();
        korean.label = "서버".into();
        korean.address = "server".into();
        assert!(korean
            .updated(&[json!({"label":"서버", "ssh":"other"})])
            .is_err());
        editor.label = "새 기기".into();
        assert!(editor.updated(&entries).is_err());
        editor.address = "user@mini.local".into();
        let saved = editor.updated(&entries).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1]["ssh"], "user@mini.local");
    }

    #[test]
    fn edits_preserve_unknown_metadata_and_reject_changed_rows() {
        let original = json!({"label":"미니", "ssh":"old", "machine_id":"stable", "home":true,
            "key":"fixture-key", "custom":{"keep":true}});
        let mut editor = Editor::existing(0, &original);
        editor.address = "http://127.0.0.1:8765/device".into();
        let saved = editor.updated(&[original.clone()]).unwrap();
        assert!(saved[0].get("ssh").is_none());
        assert_eq!(saved[0]["machine_id"], "stable");
        assert_eq!(saved[0]["custom"], original["custom"]);
        assert_eq!(saved[0]["key"], "fixture-key");
        assert!(editor.updated(&[]).is_err());
        assert!(editor
            .updated(&[json!({"label":"다른 기기", "ssh":"other"})])
            .is_err());
        let both = json!({"label":"두 연결", "base":"http://localhost:8765", "ssh":"mini"});
        let mut rename = Editor::existing(0, &both);
        rename.label = "새 이름".into();
        assert_eq!(rename.updated(&[both]).unwrap()[0]["ssh"], "mini");
    }

    #[test]
    fn ambiguous_names_and_unsafe_addresses_are_not_saved() {
        let entries = vec![json!({"label":"My Mini", "ssh":"mini"})];
        let mut editor = Editor {
            label: " my  mini ".into(),
            address: "server".into(),
            ..Default::default()
        };
        assert!(editor.updated(&entries).is_err());
        editor.label = "새 기기".into();
        for bad in [
            "",
            "-oProxyCommand=x",
            "user@host\nnext",
            "a b",
            "x;reboot",
            "ssh://host",
            "http://user:secret@host",
            "https://host/?token=secret",
            "http://host/#fragment",
            "http://[broken",
        ] {
            editor.address = bad.into();
            assert!(editor.updated(&entries).is_err(), "accepted unsafe address");
        }
    }

    #[test]
    fn deletion_is_bound_to_the_original_entry_not_just_its_index() {
        let first = json!({"label":"첫 기기", "ssh":"first"});
        let second = json!({"label":"다른 기기", "ssh":"second"});
        let mut editor = Editor::existing(0, &first);
        editor.deleting = true;
        assert_eq!(
            editor.updated(&[first.clone(), second.clone()]).unwrap(),
            vec![second.clone()]
        );
        assert!(editor.updated(&[second, first]).is_err());
    }

    #[test]
    fn a_device_identity_wins_over_a_similar_display_name() {
        let name = crate::render::pane_identity::DeviceNameRow {
            id: "local-id".into(),
            label: "My Mac".into(),
            name: None,
            local: true,
        };
        let mut machine = MachineRow {
            machine_id: Some("other-id".into()),
            label: "My Mac".into(),
            ssh: String::new(),
            status: String::new(),
            online: false,
            build_match: true,
            sync_target: String::new(),
            key: String::new(),
            guest: false,
        };
        assert!(!matches_name(&name, &machine));
        machine.machine_id = Some("~local-id".into());
        assert!(matches_name(&name, &machine));
        machine.machine_id = None;
        machine.label = " my\u{a0}mac ".into();
        assert!(matches_name(&name, &machine));
    }

    #[test]
    fn connection_summaries_do_not_reveal_url_credentials() {
        assert_eq!(
            display_address("https://name:secret@host:9000/path?token=secret#code"),
            "https://host:9000/path"
        );
        assert_eq!(display_address("user@host"), "user@host");
    }
}
