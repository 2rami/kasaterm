use super::*;
use crate::share_panel::{size, Action};
use kasa_mcp::share::shortcut::Location;

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let view = &s.share;
    section_title(g, x, *y, "공유 파일", "");
    *y += 54.0;
    plain_hint(
        g,
        x,
        y,
        w,
        "같은 공유 파일을 카사텀과 바탕화면, 폰에서 볼 수 있어요.",
    );
    let caption = if view.ready {
        format!(
            "파일 {}개 · {}",
            view.catalog.files.len(),
            size(view.catalog.status.bytes)
        )
    } else {
        "공유 파일을 확인하는 중…".into()
    };
    plain_hint(g, x, y, w, &caption);
    for (label, action) in [
        ("공유함 열기", Action::OpenPanel),
        ("폴더에서 보기", Action::Reveal),
    ] {
        button(
            g,
            s,
            hits,
            (x, *y, w.min(180.0), 26.0),
            label,
            Target::Setting(SettingsAction::Share(action)),
            false,
        );
        *y += 34.0;
    }
    section_title(g, x, *y, "이 기기의 바로가기", "");
    *y += 54.0;
    let location = match &view.preferences.location {
        Location::Desktop => "바탕화면".to_string(),
        Location::Folder(parent) => format!("{} / KASA-share", parent.display()),
        Location::Hidden => "숨김 · 앱 안에서 계속 볼 수 있어요".to_string(),
    };
    plain_hint(g, x, y, w, &location);
    if view.busy {
        plain_hint(g, x, y, w, "위치를 바꾸는 중…");
    } else {
        let inline = w >= 300.0;
        let button_w = if inline {
            (w - 16.0) / 3.0
        } else {
            w.min(180.0)
        };
        for (index, (label, action)) in [
            ("바탕화면", Action::Desktop),
            ("위치 변경", Action::ChooseLocation),
            ("숨김", Action::HideShortcut),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = if inline {
                x + index as f32 * (button_w + 8.0)
            } else {
                x
            };
            button(
                g,
                s,
                hits,
                (bx, *y, button_w, 26.0),
                label,
                Target::Setting(SettingsAction::Share(action)),
                false,
            );
            if !inline {
                *y += 34.0;
            }
        }
        if inline {
            *y += 34.0;
        }
    }
    plain_hint(g, x, y, w, "바로가기를 바꿔도 공유 파일은 이동하거나 삭제되지 않아요. 같은 이름의 다른 폴더는 덮어쓰지 않습니다.");
    if let Some(notice) = view.notice.as_deref().or(view.problem.as_deref()).or(view
        .catalog
        .status
        .shortcut_error
        .as_deref())
    {
        plain_hint(g, x, y, w, notice);
    }
    section_title(g, x, *y, "동기화", "");
    *y += 54.0;
    plain_hint(g, x, y, w, "클라우드 보관함이 아니라 기기끼리 파일을 맞춥니다. 아직 전달되지 않은 파일을 받으려면 그 파일을 가진 기기가 켜져 있어야 해요.");
    if view.catalog.status.peers.is_empty() {
        plain_hint(
            g,
            x,
            y,
            w,
            "지금 확인한 다른 기기가 없어요. 이 기기에 받은 파일은 계속 열 수 있어요.",
        );
    }
    for peer in &view.catalog.status.peers {
        plain_hint(
            g,
            x,
            y,
            w,
            &format!(
                "{} · {}",
                peer.label,
                if peer.ok {
                    "이 기기로 받기 확인"
                } else {
                    "연결 확인 필요"
                }
            ),
        );
    }
    if view.catalog.status.paused_deletes > 0 {
        plain_hint(g, x, y, w, "많은 파일이 사라져 삭제 동기화를 멈췄어요. 남은 기기의 사본을 확인하기 전에는 삭제를 승인하지 마세요.");
    }
}
