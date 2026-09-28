use super::assistant::{key_present, phase, rows, string, verified};
use super::*;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Lane {
    Answer,
    Progress,
    Verify,
    Done,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WorkKey {
    Task(String),
}
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkUi {
    pub(crate) project: Option<String>,
    pub(crate) selected: Option<WorkKey>,
    pub(crate) filters_open: bool,
    pub(crate) expanded: HashSet<Lane>,
}

impl Scene {
    pub(crate) fn select_work(&mut self, key: WorkKey) {
        self.hits.clear();
        self.work.selected = if self.work.selected.as_ref() == Some(&key) {
            None
        } else {
            Some(key)
        };
    }
}

fn task_color(task: &Value) -> [u8; 4] {
    if verified(task) {
        theme::success()
    } else {
        match string(task, "state") {
            "blocked" => theme::attention(),
            "working" | "awaiting_verification" => theme::accent(),
            _ => theme::text_dim(),
        }
    }
}
fn phase_rank(task: &Value) -> u8 {
    match string(task, "state") {
        "blocked" => 0,
        "working" => 1,
        "awaiting_verification" => 2,
        "queued" => 3,
        "verified" => 4,
        _ => 5,
    }
}
fn project_label<'a>(workspace: &'a Value, id: &str) -> &'a str {
    rows(workspace, "projects")
        .iter()
        .find(|project| string(project, "id") == id)
        .map(|project| string(project, "name"))
        .unwrap_or("아직 분류하지 않은 요청")
}

fn paint_detail(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    task: &Value,
) {
    let label = fit(g, phase(task), w, 12.0, false);
    text(g, x, *y, &label, 12.0, task_color(task), false);
    *y += 26.0;
    if !string(task, "step").is_empty() {
        overview_note(g, x, y, w, string(task, "step"), theme::text_dim());
    }
    text(g, x, *y, "원래 요청", 10.5, theme::text_dim(), false);
    *y += 22.0;
    for line in chat::wrap(string(task, "original_prompt"), w, |text| {
        g.measure_chrome_text(text, 12.0, false)
    }) {
        text(g, x, *y, &line, 12.0, theme::text(), false);
        *y += 18.0;
    }
    *y += 16.0;
    text(g, x, *y, "검증 근거", 10.5, theme::text_dim(), false);
    *y += 22.0;
    let checks = rows(task, "check_results");
    for check in rows(task, "required_checks") {
        let Some(id) = check.as_str() else {
            continue;
        };
        let state = checks
            .iter()
            .find(|check| string(check, "id") == id)
            .map(|check| string(check, "state"))
            .unwrap_or("unknown");
        let (state, color) = match state {
            "pass" => ("통과", theme::success()),
            "fail" => ("실패", theme::danger()),
            _ => ("미확인", theme::text_dim()),
        };
        let value = fit(g, &format!("{id} · {state}"), w, 10.5, false);
        text(g, x, *y, &value, 10.5, color, false);
        *y += 22.0;
    }
    if rows(task, "required_checks").is_empty() {
        overview_note(g, x, y, w, "검사 기록이 아직 없어요.", theme::text_dim());
    }
    let revision = string(task, "work_revision");
    if !revision.is_empty() {
        overview_note(
            g,
            x,
            y,
            w,
            &format!("작업 판 · {revision}"),
            theme::text_dim(),
        );
    }
    button(
        g,
        s,
        hits,
        (x, *y, 132.0_f32.min(w), 26.0),
        "이 일로 대화하기",
        Target::WorkChat(WorkKey::Task(string(task, "id").into())),
        false,
    );
    *y += 40.0;
}

pub(super) fn paint_work(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    if chat::paint_setup(g, s, hits, caret, x, y, w) {
        return;
    }
    let workspace = &s.chat.workspace;
    if s.fixture {
        overview_note(
            g,
            x,
            y,
            w,
            "검증용 가상 작업 공간 · 실제 계정이나 작업을 바꾸지 않아요.",
            theme::text_dim(),
        );
    }
    let tasks = rows(workspace, "tasks");
    let active = tasks
        .iter()
        .filter(|task| !matches!(string(task, "state"), "verified" | "cancelled"))
        .count();
    // 머리글은 판의 「작업현황」 하나다. 그 아래는 상태 한 줄과 조작만 — 큰 「프로젝트」 제목을
    // 또 세우면 이름이 세 번 겹쳐 읽힌다.
    let source = if s.chat.online {
        "계정 기록"
    } else {
        "연결 확인 중 · 마지막 기록"
    };
    let status = format!("진행 {active} · {source}");
    let project_action = if s.chat.project_editor {
        "프로젝트 접기"
    } else {
        "새 프로젝트"
    };
    // 단추는 글자를 한 자씩 재어 합으로 자른다(`info::fit_text`). 통째로 잰 폭으로 단추를
    // 만들면 그 합보다 좁아 「연결 설…」로 잘린다. 여백을 더했다 빼는 부동소수 오차도 있어 2px 를 더 둔다.
    let control_w = |g: &mut gpu::GpuRenderer, label: &str| {
        let mut buf = [0u8; 4];
        let chars: f32 = label
            .chars()
            .map(|ch| g.measure_chrome_text(ch.encode_utf8(&mut buf), 12.0, false))
            .sum();
        chars.max(g.measure_chrome_text(label, 12.0, false))
            + 2.0 * crate::native_controls::CONTROL_PADDING_X
            + 2.0
    };
    let project_w = control_w(g, project_action);
    let key_w = control_w(g, "연결 설정");
    let controls_w = project_w + 6.0 + key_w;
    let status_w = g.measure_chrome_text(&status, 11.0, false);
    let (controls_x, status_room) = if status_w + 16.0 + controls_w <= w {
        (x + w - controls_w, w - controls_w - 16.0)
    } else {
        (x, w)
    };
    let status = fit(g, &status, status_room, 11.0, false);
    text(g, x, *y + 7.0, &status, 11.0, theme::text_dim(), false);
    if controls_x == x {
        *y += 30.0;
    }
    button(
        g,
        s,
        hits,
        (controls_x, *y, project_w, 26.0),
        project_action,
        Target::AssistantProjectEdit,
        false,
    );
    button(
        g,
        s,
        hits,
        (controls_x + project_w + 6.0, *y, key_w, 26.0),
        "연결 설정",
        Target::AssistantKeyEdit,
        false,
    );
    *y += 38.0;
    if let Some(error) = &s.chat.error {
        overview_note(g, x, y, w, error, theme::danger());
    }
    let mut choices = vec![(
        "전체".into(),
        Target::WorkProject(None),
        s.work.project.is_none(),
        true,
    )];
    for project in rows(workspace, "projects") {
        let id = string(project, "id");
        choices.push((
            string(project, "name").into(),
            Target::WorkProject(Some(id.into())),
            s.work.project.as_deref() == Some(id),
            true,
        ));
    }
    overview_choices(g, s, hits, x, y, w, choices);
    if s.chat.project_editor {
        field(
            g,
            s,
            hits,
            caret,
            (x, *y, w, 40.0),
            "프로젝트 이름",
            &s.chat.project_draft,
            BoardInput::AssistantProject,
        );
        *y += 48.0;
        if !s.chat.busy {
            button(
                g,
                s,
                hits,
                (x, *y, w.min(90.0), 26.0),
                "추가",
                Target::AssistantProjectSave,
                true,
            );
        }
        *y += 38.0;
    }
    *y += 6.0;
    let mut projects = rows(workspace, "projects")
        .iter()
        .map(|project| Some(string(project, "id").to_string()))
        .collect::<Vec<_>>();
    for task in tasks {
        if let Some(id) = task.get("project").and_then(Value::as_str) {
            if !projects
                .iter()
                .any(|project| project.as_deref() == Some(id))
            {
                projects.push(Some(id.into()));
            }
        }
    }
    projects.push(None);
    let mut shown = 0;
    for project in projects {
        if s.work
            .project
            .as_ref()
            .is_some_and(|selected| project.as_ref() != Some(selected))
        {
            continue;
        }
        let mut group = tasks
            .iter()
            .filter(|task| task.get("project").and_then(Value::as_str) == project.as_deref())
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|task| {
            (
                phase_rank(task),
                std::cmp::Reverse(task.get("updated_at").and_then(Value::as_u64).unwrap_or(0)),
            )
        });
        let name = project
            .as_deref()
            .map(|id| project_label(workspace, id))
            .unwrap_or("분류 대기");
        let title = fit(g, &format!("{name}  {}", group.len()), w, 11.0, true);
        text(g, x, *y, &title, 11.0, theme::text_dim(), true);
        *y += 26.0;
        divider(g, x, *y, w);
        *y += 8.0;
        for task in group {
            shown += 1;
            let key = WorkKey::Task(string(task, "id").into());
            let selected = s.work.selected.as_ref() == Some(&key);
            let rect = (x, *y, w, 64.0);
            if selected || contains(rect, s.cursor) {
                stroke(
                    g,
                    rect,
                    if selected {
                        theme::accent()
                    } else {
                        theme::border()
                    },
                );
            }
            let title = fit(g, string(task, "goal"), (w - 20.0).max(1.0), 12.0, false);
            text(g, x + 10.0, *y + 10.0, &title, 12.0, theme::text(), false);
            let status = if string(task, "step").is_empty() {
                phase(task).into()
            } else {
                format!("{} · {}", phase(task), string(task, "step"))
            };
            let status = fit(g, &status, (w - 20.0).max(1.0), 10.5, false);
            text(
                g,
                x + 10.0,
                *y + 34.0,
                &status,
                10.5,
                task_color(task),
                false,
            );
            hit(g, hits, Target::WorkSelect(key), rect, false);
            g.hover_pointer |= contains(rect, s.cursor);
            *y += 70.0;
            if selected {
                paint_detail(g, s, hits, x + 10.0, y, (w - 20.0).max(1.0), task);
            }
        }
        *y += 18.0;
    }
    if shown == 0 {
        overview_note(
            g,
            x,
            y,
            w,
            if s.chat.loaded {
                "나쵸에게 요청을 보내면 원문과 진행 기록이 여기에 남아요. 프로젝트 분류는 확인된 기록에만 적용돼요."
            } else {
                "작업 기록을 확인하고 있어요…"
            },
            theme::text_dim(),
        );
        button(
            g,
            s,
            hits,
            (x, *y, w.min(128.0), 26.0),
            "나쵸에게 말하기",
            Target::Tab(BoardTab::Chat),
            false,
        );
        *y += 38.0;
    }
    if key_present(workspace) {
        overview_note(
            g,
            x,
            y,
            w,
            "완료 알림은 작업 판에 맞는 검사와 검증을 통과한 뒤에만 보내요.",
            theme::text_dim(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn pending_and_verified_are_not_inferred_from_free_text() {
        assert_eq!(phase_rank(&json!({"state":"queued","step":"완료"})), 3);
        assert_ne!(
            phase(&json!({"state":"verified","verify_ok":false})),
            "검증 통과"
        );
        assert_eq!(
            phase(&json!({"state":"verified","verify_ok":true})),
            "검증 통과"
        );
    }
    #[test]
    fn unknown_project_does_not_borrow_another_accounts_label() {
        let workspace = json!({"projects":[{"id":"one","name":"로컬"}]});
        assert_eq!(
            project_label(&workspace, "missing"),
            "아직 분류하지 않은 요청"
        );
    }
}
