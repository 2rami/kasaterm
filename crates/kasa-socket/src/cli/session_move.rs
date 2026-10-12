use crate::protocol::{Request, Response};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

const MACHINES_HELP: &str = "kasaterm-cli machines — 기기 목록·연결·세션 이동

  machines [--names|--json]                  등록된 기기 이름과 연결 상태
  machines connect <기기> [--here]           그 기기에 새 셸
  machines move [%N] <기기|local>            기존 claude·codex 세션 이동
  machines move --help                      이동 순서·예시·주의점";

const MOVE_HELP: &str = "kasaterm-cli machines move [%N] <기기|local> [--cwd 경로] [--run 명령] [--force]
claude·codex 세션을 다른 기기로 옮깁니다. 대화·모델을 이어서 쓰며 원래 칸은 거울이 됩니다.

  1. kasaterm-cli machines                  도착 기기의 등록된 이름과 연결 상태 확인
  2. kasaterm-cli where                     옮길 이쪽 칸 번호(%N) 확인
  3. kasaterm-cli machines move %3 맥미니   그 칸의 세션 이동

  내 세션: kasaterm-cli machines move 맥미니
  경로 지정: kasaterm-cli machines move %3 맥미니 --cwd /Users/me/project
  데려오기: kasaterm-cli machines move %3 local --cwd /Users/me/project
             %3은 이 기기에 열린 거울 칸 번호입니다. 원격 %N@기기 번호가 아닙니다.

  %N 생략      $KASATERM_PANE_ID의 내 칸. 칸 밖에서는 명시해야 합니다.
  도착 기기    machines에 나온 이름. 공백이 있으면 따옴표로 감쌉니다.
               여러 기기와 맞는 짧은 이름·알 수 없는 이름·주소는 이동 전에 거부합니다.
  --cwd 경로   도착 기기의 작업 경로. 생략하면 등록된 경로 매핑을 씁니다.
  --run 명령   도착 셸에서 실행할 명령(필요할 때만, 한 인자로 따옴표 사용). local에서는 지원하지 않습니다.
  --force      진행 중 턴 중단·도착지 충돌 덮어쓰기를 허용합니다. 기본 이동에는 쓰지 마세요.

일하는 세션은 기본적으로 턴을 마친 뒤 이동하도록 예약됩니다. 예약됨은 이동 완료가 아닙니다.
순방향은 미커밋 변경도 운반하지만 local 데려오기는 대화만 옮깁니다. 작업 파일은 별도로 확인하세요.
실패·응답 없음은 완료가 아닙니다. where·board --all·peek으로 원본과 거울 상태를 확인하세요.
확인 전 close·closed·세션 종료·--force 재시도를 하지 마세요. 주소·SSH 포트를 추측할 필요가 없습니다.
자세한 절차: docs/session-transfer.md";

pub(super) fn help_for(args: &[String]) -> Option<&'static str> {
    match args.first().map(String::as_str) {
        Some("help") if args.get(1).is_some_and(|s| s == "machines") => {
            match args.get(2).map(String::as_str) {
                None => Some(MACHINES_HELP),
                Some("move") if args.len() == 3 => Some(MOVE_HELP),
                _ => None,
            }
        }
        Some("machines") if args.get(1).is_some_and(|s| s == "move") => args[2..]
            .iter()
            .any(|s| matches!(s.as_str(), "-h" | "--help"))
            .then_some(MOVE_HELP),
        Some("machines")
            if args.len() == 2 && matches!(args[1].as_str(), "-h" | "--help" | "help") =>
        {
            Some(MACHINES_HELP)
        }
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct MoveArgs {
    pane: String,
    destination: String,
    cwd: Option<String>,
    run: Option<String>,
    force: bool,
}

fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn parse(args: &[String], current_pane: Option<&str>) -> Result<MoveArgs> {
    let (mut pane, mut destination, mut cwd, mut run) = (None, None, None, None);
    let mut force = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cwd" | "--run" => {
                let value = args
                    .next()
                    .filter(|v| {
                        !v.trim().is_empty()
                            && !v.starts_with('-')
                            && !v.chars().any(char::is_control)
                    })
                    .ok_or_else(|| {
                        anyhow!("{arg} 뒤에 한 인자로 값을 주세요 — machines move --help")
                    })?;
                let slot = if arg == "--cwd" { &mut cwd } else { &mut run };
                if slot.replace(value.clone()).is_some() {
                    bail!("{arg} 옵션은 한 번만 주세요");
                }
            }
            "--force" => {
                if force {
                    bail!("--force 옵션은 한 번만 주세요");
                }
                force = true;
            }
            _ if arg.starts_with('-') => {
                bail!("알 수 없는 이동 옵션: {arg} — machines move --help")
            }
            _ if arg.starts_with('%') => {
                if !valid_pane(arg) {
                    bail!("이쪽 칸 번호(%N)를 주세요: {arg} — where로 확인하세요. 데려오기도 이쪽 거울 번호를 씁니다");
                }
                if pane.replace(arg.clone()).is_some() {
                    bail!("옮길 칸은 한 번에 하나만 주세요");
                }
            }
            _ => {
                if arg.trim().is_empty() || arg.chars().any(char::is_control) {
                    bail!("도착 기기 이름이 비어 있거나 잘못됐어요 — machines로 확인하세요");
                }
                if destination.replace(arg.clone()).is_some() {
                    bail!("도착 기기는 하나만 주세요. 공백이 있는 이름은 따옴표로 감싸세요 — machines move --help");
                }
            }
        }
    }
    let destination = destination.ok_or_else(|| {
        anyhow!("도착 기기가 필요해요 — machines로 이름 확인 후 machines move [%N] <기기|local>")
    })?;
    let pane = pane
        .or_else(|| current_pane.filter(|p| !p.is_empty()).map(str::to_owned))
        .ok_or_else(|| {
            anyhow!("칸 밖에서는 %N을 지정하세요 — where로 확인 후 machines move %3 <기기>")
        })?;
    if !valid_pane(&pane) {
        bail!("내 칸 번호가 잘못됐어요 — where로 확인하고 %N을 지정하세요");
    }
    if destination == "local" && run.is_some() {
        bail!("local 데려오기는 --run을 지원하지 않아요");
    }
    Ok(MoveArgs {
        pane,
        destination,
        cwd,
        run,
        force,
    })
}

fn spaced(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_destination(query: &str, machines: &[Value]) -> Result<String> {
    let wanted = spaced(query);
    let label = |row: &Value| {
        row.get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let exact: Vec<&Value> = machines
        .iter()
        .filter(|m| spaced(&label(m)) == wanted)
        .collect();
    let matches: Vec<&Value> = if exact.is_empty() {
        machines
            .iter()
            .filter(|m| spaced(&label(m)).starts_with(&wanted))
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [one] if one.get("online").and_then(Value::as_bool) == Some(true) => Ok(label(one)),
        [one] => bail!("「{}」 기기에 닿지 않아요 — machines로 연결을 확인하세요. 이동 요청은 보내지 않았어요", label(one)),
        [] => bail!("「{query}」 기계가 명부에 없어요 — machines에 나온 이름을 주세요. 주소·mini 별칭으로 추측하지 않아요"),
        many => bail!("「{query}」 기계가 여럿이에요 — machines의 이름 전체로 골라 주세요:\n{}",
            many.iter().map(|m| format!("  {}", label(m))).collect::<Vec<_>>().join("\n")),
    }
}

pub(super) fn execute(
    args: &[String],
    current_pane: Option<&str>,
    mut send: impl FnMut(&Request) -> Result<Response>,
) -> Result<Response> {
    let mut options = parse(args, current_pane)?;
    if options.destination != "local" {
        let response = send(&Request {
            id: json!("move-machines"),
            method: "machine.list".into(),
            params: json!({}),
        })?;
        if !response.ok {
            bail!(
                "기기 목록을 못 읽었어요 — {}. 이동 요청은 보내지 않았어요",
                response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "machine.list 실패".into())
            );
        }
        let machines = response
            .result
            .as_ref()
            .and_then(|r| r.get("machines"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("기기 목록 응답이 잘못됐어요. 이동 요청은 보내지 않았어요"))?;
        // Passing a canonical roster name preserves the app's cwd mapping and avoids its legacy `mini` fallback.
        options.destination = resolve_destination(&options.destination, machines)?;
    }
    send(&Request {
        id: json!(format!("cli-{}", std::process::id())),
        method: "surface.migrate".into(),
        params: json!({ "pane": options.pane, "base": options.destination,
            "cwd": options.cwd, "run": options.run, "force": options.force }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_routes_do_not_require_a_pane_or_transport() {
        for words in [
            vec!["machines", "move", "--help"],
            vec!["machines", "move", "%3", "mini", "-h"],
            vec!["help", "machines", "move"],
        ] {
            let help = help_for(&args(&words)).unwrap();
            for text in [
                "machines move %3",
                "$KASATERM_PANE_ID",
                "local",
                "--cwd",
                "--run",
                "--force",
            ] {
                assert!(help.contains(text), "missing {text}");
            }
            assert!(help.contains("예약됨은 이동 완료가 아닙니다"));
            assert!(help.contains("close·closed·세션 종료"));
        }
        assert!(help_for(&args(&["machines", "--help"]))
            .unwrap()
            .contains("machines move --help"));
        assert!(help_for(&args(&["machines", "move", "mini"])).is_none());
    }

    #[test]
    fn parsing_preserves_pane_override_and_quoted_values() {
        let options = parse(
            &args(&[
                "--cwd",
                "/work/a b",
                "%3",
                "Mac Mini",
                "--run",
                "codex --resume thread",
                "--force",
            ]),
            Some("%8"),
        )
        .unwrap();
        assert_eq!(
            options,
            MoveArgs {
                pane: "%3".into(),
                destination: "Mac Mini".into(),
                cwd: Some("/work/a b".into()),
                run: Some("codex --resume thread".into()),
                force: true
            }
        );
        assert_eq!(parse(&args(&["mini"]), Some("%8")).unwrap().pane, "%8");
        assert!(parse(&args(&["mini"]), None).is_err());
    }

    #[test]
    fn malformed_arguments_never_call_transport() {
        for words in [
            vec![],
            vec!["--force"],
            vec!["mini", "--typo"],
            vec!["mini", "-f"],
            vec!["mini", "other"],
            vec!["%1", "%2", "mini"],
            vec!["%1@mini", "local"],
            vec!["%bad", "mini"],
            vec!["mini", "--cwd"],
            vec!["mini", "--cwd", "--force"],
            vec!["mini", "--run", ""],
            vec!["mini", "--cwd", "/a", "--cwd", "/b"],
            vec!["mini", "--run", "a", "--run", "b"],
            vec!["mini", "--force", "--force"],
            vec!["local", "--run", "codex"],
            vec!["mini\nother"],
            vec![""],
        ] {
            let result = execute(&args(&words), Some("%3"), |_| {
                panic!("invalid input contacted transport: {words:?}")
            });
            assert!(result.is_err(), "accepted {words:?}");
        }
    }

    #[test]
    fn unsafe_destination_never_sends_migration_even_with_force() {
        for (query, machines) in [
            ("unknown", json!([{ "label": "mini", "online": true }])),
            (
                "mini",
                json!([{ "label": "mini-work", "online": true }, { "label": "mini-home", "online": true }]),
            ),
            (
                "mini",
                json!([{ "label": "mini", "online": true }, { "label": "mini", "online": true }]),
            ),
            ("mini", json!([{ "label": "mini", "online": false }])),
            ("mini", json!([{ "label": "mini" }])),
            ("mini", json!([{ "label": "Desktop", "online": true }])),
            (
                "http://127.0.0.1:1234",
                json!([{ "label": "mini", "online": true }]),
            ),
        ] {
            let mut calls = Vec::new();
            let result = execute(&args(&["%3", query, "--force"]), None, |request| {
                calls.push(request.method.clone());
                assert_eq!(request.method, "machine.list");
                Ok(Response::success(
                    request.id.clone(),
                    json!({ "machines": machines }),
                ))
            });
            assert!(result.is_err(), "accepted {query}");
            assert_eq!(calls, ["machine.list"]);
        }
    }

    #[test]
    fn list_failure_or_malformed_response_stops_before_migration() {
        for response in [
            Response::error(json!("x"), -32601, "unsupported"),
            Response::success(json!("x"), json!({})),
        ] {
            let mut calls = 0;
            assert!(execute(&args(&["mini"]), Some("%3"), |request| {
                calls += 1;
                assert_eq!(request.method, "machine.list");
                Ok(response.clone())
            })
            .is_err());
            assert_eq!(calls, 1);
        }
        assert!(execute(&args(&["mini"]), Some("%3"), |_| Err(anyhow!(
            "transport unavailable"
        )))
        .is_err());
    }

    #[test]
    fn canonical_name_is_resolved_before_exactly_one_migration() {
        let mut calls = Vec::new();
        let result = execute(
            &args(&["%3", "Mac Mini", "--cwd", "/work/a b"]),
            Some("%8"),
            |request| {
                calls.push(request.clone());
                Ok(if request.method == "machine.list" {
                    Response::success(
                        request.id.clone(),
                        json!({ "machines": [
                    { "label": "Mac\u{a0}Mini", "online": true },
                    { "label": "Mac Mini Work", "online": true }
                ] }),
                    )
                } else {
                    Response::success(request.id.clone(), json!({ "remote_id": "%17" }))
                })
            },
        )
        .unwrap();
        assert!(result.ok);
        assert_eq!(
            calls.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(),
            ["machine.list", "surface.migrate"]
        );
        assert_eq!(
            calls[1].params,
            json!({ "pane": "%3", "base": "Mac\u{a0}Mini", "cwd": "/work/a b", "run": null, "force": false })
        );
        assert_eq!(
            resolve_destination("Mac", &[json!({ "label": "Mac Mini", "online": true })]).unwrap(),
            "Mac Mini"
        );
    }

    #[test]
    fn reverse_migration_uses_local_mirror_id_without_machine_lookup() {
        let mut calls = Vec::new();
        execute(
            &args(&["%12", "local", "--cwd", "/work"]),
            None,
            |request| {
                calls.push(request.clone());
                Ok(Response::success(
                    request.id.clone(),
                    json!({ "remote_id": "%12" }),
                ))
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "surface.migrate");
        assert_eq!(
            calls[0].params,
            json!({ "pane": "%12", "base": "local", "cwd": "/work", "run": null, "force": false })
        );
    }

    #[test]
    fn failed_or_uncertain_migration_is_not_retried_or_closed() {
        for fail_transport in [false, true] {
            let mut calls = Vec::new();
            let result = execute(&args(&["mini"]), Some("%3"), |request| {
                calls.push(request.method.clone());
                if request.method == "machine.list" {
                    Ok(Response::success(
                        request.id.clone(),
                        json!({ "machines": [{ "label": "mini", "online": true }] }),
                    ))
                } else if fail_transport {
                    Err(anyhow!("response timeout"))
                } else {
                    Ok(Response::error(
                        request.id.clone(),
                        -32603,
                        "destination refused",
                    ))
                }
            });
            if fail_transport {
                assert!(result.is_err());
            } else {
                assert!(!result.unwrap().ok);
            }
            assert_eq!(calls, ["machine.list", "surface.migrate"]);
        }
    }
}
