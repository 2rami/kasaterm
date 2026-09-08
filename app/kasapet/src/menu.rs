use kasa_pet_config::{PetPreferences, PreferenceChange};

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
#[derive(Clone)]
pub enum Item {
    Action(Action, bool),
    Page(usize),
    Heading,
}
#[derive(Clone)]
pub struct Row {
    pub label: String,
    pub enabled: bool,
    pub checked: Option<bool>,
    pub item: Item,
}
pub struct Content {
    pub pages: Vec<(String, Vec<Row>)>,
}
fn action(label: impl Into<String>, enabled: bool, value: Action) -> Row {
    Row {
        label: label.into(),
        enabled,
        checked: None,
        item: Item::Action(value, false),
    }
}
fn check(label: impl Into<String>, enabled: bool, checked: bool, value: Action) -> Row {
    Row {
        label: label.into(),
        enabled,
        checked: Some(checked),
        item: Item::Action(value, true),
    }
}
fn page(label: &str, index: usize) -> Row {
    Row {
        label: label.into(),
        enabled: true,
        checked: None,
        item: Item::Page(index),
    }
}
fn heading(label: &str) -> Row {
    Row {
        label: label.into(),
        enabled: false,
        checked: None,
        item: Item::Heading,
    }
}

pub fn content(
    prefs: &PetPreferences,
    resting: bool,
    typing: bool,
    chatting: bool,
    can_touch: bool,
    can_next: bool,
    can_save: bool,
    catalog: &crate::catalog::Catalog,
    playback: &crate::catalog::Playback,
    expressions: &[usize],
) -> Content {
    let home = vec![
        action(
            if chatting {
                "나쵸 대화 닫기"
            } else {
                "나쵸와 대화"
            },
            true,
            Action::Chat,
        ),
        action(
            "재시작 확인할 일",
            true,
            Action::ChatAsk("나 재시작하면 뭐 확인해야 돼?"),
        ),
        heading("펫"),
        page("모션", 1),
        page("표정 · 소품", 2),
        page("빠른 설정", 3),
        heading("더 보기"),
        page("다른 행동 · 작업 기록", 4),
        action("펫 끄기", true, Action::Quit),
    ];
    let mut motions: Vec<Row> = catalog
        .motions
        .iter()
        .enumerate()
        .map(|(i, m)| {
            check(
                if catalog
                    .motions
                    .iter()
                    .filter(|other| other.group == m.group)
                    .count()
                    > 1
                {
                    m.label()
                } else {
                    format!(
                        "{}{}",
                        m.label().split(" · ").next().unwrap_or(&m.group),
                        if m.available { "" } else { " (파일 없음)" }
                    )
                },
                m.available,
                playback.selected == Some(i),
                Action::Motion(i),
            )
        })
        .collect();
    motions.push(check(
        "선택한 모션 반복",
        true,
        playback.repeat,
        Action::RepeatMotion,
    ));
    motions.push(check(
        "자동 동작",
        true,
        playback.selected.is_none(),
        Action::Automatic,
    ));
    let mut effects: Vec<Row> = catalog
        .expressions
        .iter()
        .enumerate()
        .map(|(i, e)| {
            check(
                format!(
                    "{}{}",
                    e.label().split(" · ").next().unwrap_or(&e.name),
                    if e.unlinked {
                        " (모델 연결 없음)"
                    } else if !e.available {
                        " (파일 없음)"
                    } else {
                        ""
                    }
                ),
                e.available && !e.unlinked,
                expressions.contains(&i),
                Action::Expression(i),
            )
        })
        .collect();
    effects.push(check(
        "기본 표정으로",
        true,
        expressions.is_empty(),
        Action::ResetExpressions,
    ));
    effects.push(heading("겹치는 효과는 자동으로 바뀝니다"));
    let settings = [
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
    ]
    .into_iter()
    .map(|(s, on, p)| check(s, can_save, on, Action::Preference(p)))
    .collect();
    let more = vec![
        action(
            if typing {
                "학생 말 걸기 닫기"
            } else {
                "학생에게 말 걸기"
            },
            true,
            Action::Talk,
        ),
        action("쓰다듬기", can_touch, Action::Touch),
        action(if resting { "깨우기" } else { "쉬기" }, true, Action::Rest),
        action("다음 캐릭터", can_next, Action::Next),
        heading("작업 기록"),
        action(
            "시킨 일 요약",
            true,
            Action::ChatAsk("내가 시킨 일들을 요약해줘."),
        ),
        action(
            "반영 기다리는 일",
            true,
            Action::ChatAsk("반영을 기다리는 일은 뭐야?"),
        ),
        action(
            "요청 기록 열기",
            true,
            Action::Journal(crate::journal::Action::Open),
        ),
    ];
    Content {
        pages: vec![
            ("펫".into(), home),
            ("모션".into(), motions),
            ("표정 · 소품".into(), effects),
            ("빠른 설정".into(), settings),
            ("다른 행동 · 작업 기록".into(), more),
        ],
    }
}

#[cfg(target_os = "macos")]
#[path = "menu_popup.rs"]
mod popup;
#[cfg(target_os = "macos")]
pub use popup::Popup;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pages_preserve_actions_and_keep_effects_open() {
        let mut catalog = crate::catalog::Catalog::default();
        catalog.expressions.push(crate::catalog::Expression {
            name: "blush".into(),
            file: "blush.exp3.json".into(),
            path: Default::default(),
            parameters: Default::default(),
            available: true,
            unlinked: true,
        });
        let data = content(
            &PetPreferences::default(),
            false,
            false,
            false,
            true,
            true,
            true,
            &catalog,
            &crate::catalog::Playback::default(),
            &[],
        );
        assert!(matches!(
            data.pages[0].1[0].item,
            Item::Action(Action::Chat, false)
        ));
        assert!(matches!(
            data.pages[0].1[1].item,
            Item::Action(Action::ChatAsk(_), false)
        ));
        assert!(!data.pages[2].1[0].enabled);
        assert!(data.pages[2].1[0].label.contains("모델 연결 없음"));
        assert!(data.pages[3]
            .1
            .iter()
            .all(|row| matches!(row.item, Item::Action(Action::Preference(_), true))));
        assert!(data.pages[4]
            .1
            .iter()
            .any(|row| matches!(row.item, Item::Action(Action::Talk, false))));
    }
}
