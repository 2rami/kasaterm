use kasa_pet_config::{PetPreferences, PreferenceChange};
use muda::{CheckMenuItem, ContextMenu, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[derive(Clone, Copy)]
pub enum Action {
    Chat,
    ChatAsk(&'static str),
    Talk,
    Touch,
    Rest,
    Next,
    Quit,
    Preference(PreferenceChange),
    Motion(usize),
    Expression(usize),
    ResetExpressions,
    RepeatMotion,
    Automatic,
    Journal(crate::journal::Action),
}

pub fn show(
    win: &Window,
    prefs: &PetPreferences,
    resting: bool,
    typing: bool,
    chatting: bool,
    can_touch: bool,
    can_next: bool,
    can_save: bool,
    catalog: &crate::catalog::Catalog,
    playback: &crate::catalog::Playback,
    active_expressions: &[usize],
) -> Option<Action> {
    let menu = Menu::new();
    let mut actions: Vec<(MenuId, Action)> = Vec::new();
    for (title, enabled, action) in [
        (if chatting { "나쵸 대화 닫기" } else { "나쵸와 대화" }, cfg!(target_os = "macos"), Action::Chat),
        ("재시작 확인할 일", cfg!(target_os = "macos"), Action::ChatAsk("나 재시작하면 뭐 확인해야 돼?")),
        (
            if typing {
                "학생 말 걸기 닫기"
            } else {
                "학생에게 말 걸기"
            },
            true,
            Action::Talk,
        ),
        ("쓰다듬기", can_touch, Action::Touch),
        (if resting { "깨우기" } else { "쉬기" }, true, Action::Rest),
        ("다음 캐릭터", can_next, Action::Next),
    ] {
        let item = MenuItem::new(title, enabled, None);
        actions.push((item.id().clone(), action));
        menu.append(&item).ok()?;
    }
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for (title, action) in [
        ("시킨 일 요약", Action::ChatAsk("내가 시킨 일들을 요약해줘.")),
        ("반영 기다리는 일", Action::ChatAsk("반영을 기다리는 일은 뭐야?")),
        ("요청 기록 열기", Action::Journal(crate::journal::Action::Open)),
    ] {
        let item = MenuItem::new(title, true, None);
        actions.push((item.id().clone(), action));
        menu.append(&item).ok()?;
    }
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    let motions = Submenu::new("모션", !catalog.motions.is_empty());
    for (i, motion) in catalog.motions.iter().enumerate() {
        let item = CheckMenuItem::new(
            motion.label(),
            motion.available,
            playback.selected == Some(i),
            None,
        );
        actions.push((item.id().clone(), Action::Motion(i)));
        motions.append(&item).ok()?;
    }
    menu.append(&motions).ok()?;
    let expressions = Submenu::new("표정·소품", !catalog.expressions.is_empty());
    let reset = MenuItem::new("기본 표정", !active_expressions.is_empty(), None);
    actions.push((reset.id().clone(), Action::ResetExpressions));
    expressions.append(&reset).ok()?;
    expressions
        .append(&MenuItem::new(
            "겹치는 효과는 자동으로 바뀝니다",
            false,
            None,
        ))
        .ok()?;
    expressions.append(&PredefinedMenuItem::separator()).ok()?;
    for (i, expression) in catalog.expressions.iter().enumerate() {
        let item = CheckMenuItem::new(
            expression.label(),
            expression.available && !expression.unlinked,
            active_expressions.contains(&i),
            None,
        );
        actions.push((item.id().clone(), Action::Expression(i)));
        expressions.append(&item).ok()?;
    }
    menu.append(&expressions).ok()?;
    let repeat = CheckMenuItem::new("선택한 모션 반복", true, playback.repeat, None);
    actions.push((repeat.id().clone(), Action::RepeatMotion));
    menu.append(&repeat).ok()?;
    let automatic = MenuItem::new("자동 동작으로 돌아가기", true, None);
    actions.push((automatic.id().clone(), Action::Automatic));
    menu.append(&automatic).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for (title, checked, change) in [
        (
            "말풍선 표시",
            prefs.bubbles,
            PreferenceChange::Bubbles(!prefs.bubbles),
        ),
        (
            "시선 따라가기",
            prefs.follow_cursor,
            PreferenceChange::FollowCursor(!prefs.follow_cursor),
        ),
        (
            "자동 움직임",
            prefs.animations,
            PreferenceChange::Animations(!prefs.animations),
        ),
        (
            "작업 상태에 반응",
            prefs.activity_reactions,
            PreferenceChange::ActivityReactions(!prefs.activity_reactions),
        ),
        (
            "항상 위에 표시",
            prefs.always_on_top,
            PreferenceChange::AlwaysOnTop(!prefs.always_on_top),
        ),
        (
            "위치 고정",
            prefs.lock_position,
            PreferenceChange::LockPosition(!prefs.lock_position),
        ),
    ] {
        let item = CheckMenuItem::new(title, can_save, checked, None);
        actions.push((item.id().clone(), Action::Preference(change)));
        menu.append(&item).ok()?;
    }
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    let quit = MenuItem::new("펫 끄기", true, None);
    actions.push((quit.id().clone(), Action::Quit));
    menu.append(&quit).ok()?;
    let handle = win.window_handle().ok()?;
    // Native tracking keeps keyboard navigation and dismissal without activating
    // the pet merely to show its menu. Only Talk requests the keyboard afterward.
    unsafe {
        match handle.as_raw() {
            #[cfg(target_os = "macos")]
            RawWindowHandle::AppKit(h) => {
                menu.show_context_menu_for_nsview(h.ns_view.as_ptr().cast(), None);
            }
            #[cfg(target_os = "windows")]
            RawWindowHandle::Win32(h) => {
                menu.show_context_menu_for_hwnd(h.hwnd.get(), None);
            }
            _ => return None,
        }
    }
    let mut selected = None;
    while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
        if let Some((_, action)) = actions.iter().find(|(id, _)| *id == event.id) {
            selected = Some(*action);
        }
    }
    selected
}
