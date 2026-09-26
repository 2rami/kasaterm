"""기기에서 받기·설치 예약·검증 — 앱 재시작의 사실·신뢰 계약을 다시 써서, 기기마다 무엇이 되고 무엇이 막혔는지 보인다.

지금은 dry-run 뿐이다. 기기 앱에 「받기·설치 예약」 창구가 아직 없다 — 그 창구가 든 판을 사람이 한 번 설치해야 생긴다.
그래서 모든 기기가 `update_endpoint_missing` 으로 막히고, 나머지 사유(바쁜 학생·미저장 편집기·서명·OS)는 그 창구가
생긴 뒤에도 그대로 먹는 검사다. 사실은 `kasaterm-cli app-restart plan --json`(docs/app-restart.md)에서 읽는다 —
명부에 든 기기만, 그 기기 앱이 스스로 잰 값이다.

창구가 생기면 기기가 할 일(여기서 보이는 여섯 단계):
1 받기     릴리스 산출물을 그 기기 캐시로(`~/Library/Caches/kasaterm/updates/<tag>/`) — 소스·빌드 환경 없이.
2 확인     크기·sha256(릴리스 단계가 잰 값) · EdDSA(저장소 공개키) · mac 은 dmg 를 읽기 전용으로 열어 서명이 설치본과 같은 팀인지.
3 준비     mac 은 확인한 번들을 곁(`.kasaterm.app.next`)에 두고, 윈도는 확인한 msi 를 둔다. 설치본은 아직 안 건드린다.
4 예약     종료할 때 바꾼다 — 자기설치와 같은 규칙(이전 판을 `.kasaterm.app.previous` 로, 실패하면 되돌림).
5 재시작   앱 재시작 계약 그대로 — 별도 `kasaterm_restart` 승인, 바쁘거나 미저장이면 기다린다(강제 종료 없음).
6 검증     `/version` 이 목표 버전·태그 커밋이면 끝, 아니면 이전 판을 둔 채 실패로 적는다.
"""

import json
import os
from pathlib import Path
import shutil

UPDATE_CAPABILITY = 1
CLI_CANDIDATES = (str(Path.home() / "Applications/kasaterm.app/Contents/MacOS/kasaterm-cli"),
                  "/Applications/kasaterm.app/Contents/MacOS/kasaterm-cli")
# 재시작 쪽 사유 중 「기다리면 풀리는 것」 — 받기·준비는 해 둘 수 있고 적용만 미룬다.
WAITS = {"busy_students", "unsaved_editors", "job_in_flight", "stale_facts", "bake_in_progress"}


def cli_path(which=shutil.which):
    return which("kasaterm-cli") or next((c for c in CLI_CANDIDATES if os.path.exists(c)), None)


def gather_facts(runner, ids, cli=None):
    """{machine_id: target} — 앱 재시작 계획의 대상 칸(facts·refusals) 그대로. 읽기만."""
    cli = cli or cli_path()
    if not cli:
        return {}, "kasaterm-cli 를 찾지 못했다"
    if not ids:
        return {}, None
    r = runner.run([cli, "app-restart", "plan", "--machine", ",".join(ids), "--json"], timeout=60)
    if not r.ok:
        return {}, f"앱 재시작 계획을 못 읽었다 — {r.tail(2) or '시간 초과'}"
    try:
        doc = json.loads(r.out)
    except ValueError:
        return {}, "앱 재시작 계획 응답을 못 읽었다"
    return {t["machine_id"]: t for t in doc.get("targets", [])}, None


def artifact_for(plan, state, osname):
    platform = {"macos": "macos", "windows": "windows"}.get(osname)
    got = ((state.get("stages") or {}).get("release", {}).get("detail") or {}).get("assets") or {}
    asset = got.get(platform) if isinstance(got, dict) else None
    if not platform:
        return None
    from tools.release.backend import asset_names
    name = asset_names(plan["tag"])[platform]
    url = f"https://github.com/2rami/kasaterm/releases/download/{plan['tag']}/{name}"
    if not asset:
        return {"name": name, "url": url, "verified": False}
    return {"name": name, "url": url, "verified": True, "size": asset["size"], "sha256": asset["sha256"]}


def plan_devices(plan, state, targets, facts_problem=None):
    """기기마다 {상태, 사유, 여섯 단계}. 상태: ready(창구가 있고 막힘 없음) · deferred(받기·준비는 되고 적용만 기다림) · blocked."""
    labels = {d.get("machine_id"): d["label"] for d in plan.get("baseline", []) if d.get("machine_id")}
    mac_block = next((b for b in plan.get("live_blocks", []) if b.startswith("mac 서명")), None)
    team = ((plan.get("signing") or {}).get("installed") or {}).get("team")
    rows = []
    for mid in plan["device_ids"]:
        t = targets.get(mid)
        facts = (t or {}).get("facts") or {}
        osname = facts.get("os")
        reasons = []
        if not t or not facts:
            reasons.append(("unreachable", facts_problem or "앱 재시작 사실을 못 읽었다 — 명부에 없거나 닿지 않는다"))
        if facts and int(facts.get("update_capability") or 0) < UPDATE_CAPABILITY:
            reasons.append(("update_endpoint_missing", "기기 앱에 받기·설치 예약 창구가 없다 — 창구가 든 판을 한 번 사람이 설치해야 한다"))
        if osname == "macos" and mac_block:
            reasons.append(("signing", mac_block))
        for r in (t or {}).get("refusals") or []:
            code = r.get("code")
            if code == "unsupported_os" and osname == "windows":
                continue  # 윈도는 재시작 대신 WinSparkle 이 MSI 를 돌린다 — 적용은 사람이 [설치]
            if code == "unreachable" and any(c == "unreachable" for c, _ in reasons):
                reasons[:] = [(c, r.get("reason") or w) if c == "unreachable" else (c, w) for c, w in reasons]
                continue
            reasons.append((code, "앱 재시작 쪽 사유 — 적용을 미룬다" if code in WAITS else f"앱 재시작 쪽 사유{': ' + r['reason'] if r.get('reason') else ''}"))
        art = artifact_for(plan, state, osname)
        if art and not art["verified"]:
            reasons.append(("artifacts_unverified", "릴리스 단계가 산출물을 아직 확인하지 않았다"))
        hard = [c for c, _ in reasons if c not in WAITS]
        status = "blocked" if hard else ("deferred" if reasons else "ready")
        rows.append({"machine_id": mid, "label": labels.get(mid, mid), "os": osname, "status": status,
                     "reasons": [{"code": c, "why": w} for c, w in reasons],
                     "steps": steps(plan, osname, art, team, facts),
                     "fallback": f"그 기기에서 판 번호 줄을 누르거나 {art['url'] if art else 'https://github.com/2rami/kasaterm/releases/latest'} 에서 받기"})
    return rows


def steps(plan, osname, art, team, facts):
    target = f"{plan['version']} · {plan['tag']} 의 버전 커밋"
    if osname not in ("macos", "windows"):
        return ["OS 를 몰라 산출물·단계를 정하지 못했다 — 앱 재시작 사실이 닿으면 다시 본다"]
    name = art["name"] if art else "(이 OS 의 산출물 없음)"
    digest = f"{art.get('size')}바이트 · {art.get('sha256', '')[:19]}" if art and art.get("verified") else "릴리스 단계가 잰 크기·해시"
    if osname == "windows":
        return [f"받기: {name} → %LOCALAPPDATA%\\kasaterm\\updates\\{plan['tag']}\\",
                f"확인: {digest} · EdDSA(저장소 공개키)",
                "준비: 확인한 msi 를 둔다(설치본 안 건드림)",
                "예약: WinSparkle 토스트 [설치] — 사람이 누를 때 MSI 실행",
                "재시작: MSI 가 앱을 닫고 다시 연다(도는 학생이 있으면 사람이 고른다)",
                f"검증: /version 이 {target}"]
    return [f"받기: {name} → ~/Library/Caches/kasaterm/updates/{plan['tag']}/",
            f"확인: {digest} · EdDSA(저장소 공개키) · dmg 읽기 전용 · codesign --verify --deep --strict · 팀 {team or '미확인'} 과 같음 · 공증",
            f"준비: 확인한 번들을 {Path(facts.get('app_path') or '~/Applications/kasaterm.app').with_name('.kasaterm.app.next')} 에",
            "예약: 종료할 때 바꾼다 — 이전 판을 .kasaterm.app.previous 로, 실패하면 되돌림(자기설치 규칙)",
            "재시작: 앱 재시작 계약 — 별도 kasaterm_restart 승인, 바쁘거나 미저장이면 기다림(강제 종료 없음)",
            f"검증: /version 이 {target} — 아니면 이전 판을 둔 채 실패로 적는다"]


def job_spec(plan, row, art):
    """창구가 생기면 기기에 보낼 작업 — 지금은 보이기만 한다. 기기는 이 값과 자기 사실·나쵸 승인으로만 판정한다."""
    return {"schema": "kasaterm-update/1", "plan": plan["plan_id"], "machine_id": row["machine_id"], "tag": plan["tag"],
            "version": plan["version"], "commit": plan["commit"], "asset": art,
            "required_team": ((plan.get("signing") or {}).get("installed") or {}).get("team"),
            "ed_public_key": plan["capabilities"]["macos"].get("ed_public_key")}
