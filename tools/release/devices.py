"""기기에서 받기·확인·준비·적용 — 기기 앱의 업데이트 창구(`kasa_socket::app_update`, docs/app-update.md)에 보낼 작업을
짓고, 기기마다 무엇이 되고 무엇이 막혔는지 보인다. 사실은 `kasaterm-cli app-restart plan --json` 에서 읽는다 — 명부에 든
기기만, 그 기기 앱이 스스로 잰 값이다.

기기가 하는 일(여기서 보이는 여섯 단계):
1 받기     공식 피드·공식 릴리스의 그 태그 dmg 하나만(https·넘겨주기도 https·크기 상한). 요청은 주소를 싣지 못한다.
2 확인     sha256(릴리스 단계가 잰 값) · EdDSA(설치본의 Sparkle 공개키) · dmg 읽기 전용 · 서명 팀이 설치본과 같음 · 공증 · 판 번호.
3 준비     확인한 번들을 곁(`.kasaterm.app.next`)에 둔다. 설치본은 아직 안 건드린다.
4 적용     바쁜 학생·미저장 편집기가 없을 때만 — 앱이 스스로 끄고 도우미가 갈아 끼운다(이전 판은 `.kasaterm.app.previous`).
5 재기동   도우미가 다시 띄운다. 새 판이 부팅 표식을 못 남기고 꺼지면 이전 판을 되돌려 다시 띄운다(강제 종료 없음).
6 검증     부팅 표식의 빌드가 태그 커밋이면 끝, 아니면 실패로 적는다.

창구는 이 판부터 기기 앱에 있다(`update_capability`). 그래도 설치 실행은 기기마다 기본 꺼짐(`update_enabled`,
`KASATERM_APP_UPDATE=on`)이고, 작업은 나쵸 `kasaterm_update` 승인을 조종 기기가 한 번 소비해야 받아들여진다 — 그 승인
동작이 나쵸에 아직 없어서 지금은 작업을 **지어 보이기만** 한다.
"""

import json
import os
from pathlib import Path
import shutil

UPDATE_CAPABILITY = 1
UPDATE_SCHEMA = "kasaterm-update/1"
UPDATE_ACTION = "kasaterm_update"
MAX_ASSET_BYTES = 512 * 1024 * 1024
RELEASE_PREFIX = "https://github.com/2rami/kasaterm/releases/download/"
CLI_CANDIDATES = (str(Path.home() / "Applications/kasaterm.app/Contents/MacOS/kasaterm-cli"),
                  "/Applications/kasaterm.app/Contents/MacOS/kasaterm-cli")
# 재시작 쪽 사유 중 「기다리면 풀리는 것」 — 받기·준비는 해 둘 수 있고 적용만 미룬다.
WAITS = {"busy_students", "unsaved_editors", "job_in_flight", "stale_facts", "bake_in_progress"}


def cli_path(which=shutil.which):
    return which("kasaterm-cli") or next((c for c in CLI_CANDIDATES if os.path.exists(c)), None)


def gather_facts(runner, ids, cli=None):
    """{machine_id: target} — 앱 재시작 계획의 대상 칸(facts·refusals·hash) 그대로. 읽기만."""
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


def asset_url(tag, name):
    return f"{RELEASE_PREFIX}{tag}/{name}"


def artifact_for(plan, state, osname):
    platform = {"macos": "macos", "windows": "windows"}.get(osname)
    got = ((state.get("stages") or {}).get("release", {}).get("detail") or {}).get("assets") or {}
    asset = got.get(platform) if isinstance(got, dict) else None
    if not platform:
        return None
    from tools.release.backend import asset_names
    name = asset_names(plan["tag"])[platform]
    url = asset_url(plan["tag"], name)
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
            reasons.append(("update_endpoint_missing", "기기 앱에 받기·설치 창구가 없다 — 창구가 든 판을 한 번 사람이 설치해야 한다"))
        elif facts and osname == "macos" and not facts.get("update_enabled"):
            reasons.append(("update_disabled", "기기의 설치 스위치가 꺼져 있다(KASATERM_APP_UPDATE) — 켜는 것은 그 기기 사람의 결정"))
        if osname == "macos" and mac_block:
            reasons.append(("signing", mac_block))
        for r in (t or {}).get("refusals") or []:
            code = r.get("code")
            if code == "unsupported_os" and osname == "windows":
                continue  # 윈도는 이 창구 대신 WinSparkle 이 MSI 를 돌린다 — 적용은 사람이 [설치]
            if code == "capability_missing":
                continue  # 재시작 창구 판 — 업데이트는 update_capability 로 따로 본다
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
    target = f"{plan['version']} · {plan['tag']} 의 커밋"
    if osname not in ("macos", "windows"):
        return ["OS 를 몰라 산출물·단계를 정하지 못했다 — 앱 재시작 사실이 닿으면 다시 본다"]
    name = art["name"] if art else "(이 OS 의 산출물 없음)"
    digest = f"{art.get('size')}바이트 · {art.get('sha256', '')[:19]}" if art and art.get("verified") else "릴리스 단계가 잰 크기·해시"
    if osname == "windows":
        return [f"받기: {name} → %LOCALAPPDATA%\\kasaterm\\updates\\{plan['tag']}\\",
                f"확인: {digest} · EdDSA(저장소 공개키)",
                "준비: 확인한 msi 를 둔다(설치본 안 건드림)",
                "적용: WinSparkle 토스트 [설치] — 사람이 누를 때 MSI 실행",
                "재기동: MSI 가 앱을 닫고 다시 연다(도는 학생이 있으면 사람이 고른다)",
                f"검증: /version 이 {target}"]
    return [f"받기: {name} → ~/Library/Caches/kasaterm/updates/ — 공식 피드·공식 릴리스만, https, 크기 상한",
            f"확인: {digest} · EdDSA(설치본 Sparkle 공개키) · dmg 읽기 전용 · codesign --verify --deep --strict · 팀 {team or '미확인'} 과 같음 · 공증 · 판 번호",
            f"준비: 확인한 번들을 {Path(facts.get('app_path') or '~/Applications/kasaterm.app').with_name('.kasaterm.app.next')} 에(설치본 안 건드림)",
            "적용: 바쁜 학생·미저장 편집기가 없을 때 앱이 스스로 끄고 도우미가 갈아 끼운다 — 이전 판은 .kasaterm.app.previous",
            "재기동: 도우미가 다시 띄운다 — 새 판이 부팅 표식 없이 꺼지면 이전 판을 되돌려 다시 띄움(강제 종료 없음)",
            f"검증: 부팅 표식의 빌드가 {target} — 아니면 실패로 적는다"]


def fnv(parts):
    """`kasa_socket::app_restart::fnv` 와 같은 값 — 작업 id 를 기기와 조종 쪽이 따로 지어도 같아야 한다."""
    h = 0xcbf29ce484222325
    for part in parts:
        for b in part.encode():
            h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
        h = ((h ^ 0x1f) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def target_hash(facts):
    """`kasa_socket::app_restart::target_hash` 와 같은 값 — 기기 정체(pid·바이너리·자기설치 예정)가 계획 때와 같은지 재는 표."""
    b = facts.get("binary") or {}
    p = facts.get("install_pending")
    pending = f"{p['dist_path']}@{p['dist_mtime_ms']}" if p else ""
    return fnv([facts["machine_id"], facts["app_path"], str(facts["pid"]), str(b.get("inode", 0)), str(b.get("mtime_ms", 0)),
                b.get("build", ""), pending, str(facts.get("capability", 0))])


def update_job_id(plan_hash, machine_id, sha256):
    return "up" + fnv([plan_hash, machine_id, sha256])


def update_job(plan, state, target, now_ms, plan_hash=None):
    """(작업, 못 짓는 까닭) — 기기의 `check_job` 이 받는 모양 그대로. 주소는 태그에서 지은 공식 주소뿐이다.
    `plan_hash` 는 rollout id(`update_rollout`) — 없으면 릴리스 plan_id(보이기용)."""
    facts = (target or {}).get("facts") or {}
    if facts.get("os") != "macos":
        return None, "mac 만 이 창구로 받는다 — 윈도는 WinSparkle"
    art = artifact_for(plan, state, "macos")
    if not art or not art["verified"]:
        return None, "릴리스 단계가 mac 산출물을 확인하지 않았다"
    signature = (((state.get("stages") or {}).get("feed", {}).get("detail") or {}).get("signatures") or {}).get("macos")
    if not signature:
        return None, "피드 단계가 mac EdDSA 서명을 확인하지 않았다"
    team = ((plan.get("signing") or {}).get("installed") or {}).get("team")
    if not team:
        return None, "설치본 서명 팀을 모른다"
    if not art["size"] or art["size"] > MAX_ASSET_BYTES:
        return None, "산출물 크기가 상한 밖이다"
    mid = target["machine_id"]
    plan_hash = plan_hash or plan["plan_id"]
    return {"schema": UPDATE_SCHEMA, "job_id": update_job_id(plan_hash, mid, art["sha256"]), "plan_hash": plan_hash,
            "machine_id": mid, "target_hash": target.get("hash") or "", "tag": plan["tag"], "version": plan["version"],
            "commit": plan["commit"], "build": plan["commit"],
            "asset": {"name": art["name"], "url": art["url"], "size": art["size"], "sha256": art["sha256"], "ed_signature": signature},
            "team": team, "require_notarized": True, "old_pid": int(facts.get("pid") or 0), "created_at_ms": now_ms}, None


def update_scope(job, controller, order=1):
    """나쵸가 승인할 범위 — `kasa_socket::app_update::scope` 와 같은 모양."""
    return {"action": UPDATE_ACTION, "plan": job["plan_hash"], "controller": controller, "tag": job["tag"],
            "version": job["version"], "commit": job["commit"],
            "asset": {"name": job["asset"]["name"], "sha256": job["asset"]["sha256"], "size": job["asset"]["size"]},
            "targets": [{"order": order, "machine_id": job["machine_id"], "hash": job["target_hash"]}]}


def rollout_scope(jobs, controller):
    """대상 여럿을 한 승인으로 — `kasa_socket::app_update::rollout_scope` 와 같은 키 8개(나쵸 `validate_update_scope`)."""
    first = jobs[0]
    return {"action": UPDATE_ACTION, "plan": first["plan_hash"], "controller": controller, "tag": first["tag"],
            "version": first["version"], "commit": first["commit"],
            "asset": {"name": first["asset"]["name"], "sha256": first["asset"]["sha256"], "size": first["asset"]["size"]},
            "targets": [{"order": n, "machine_id": j["machine_id"], "hash": j["target_hash"]} for n, j in enumerate(jobs, 1)]}


def rollout_id(release_plan, sha256, targets, created_at_ms):
    """이번 굴림의 id — 같은 릴리스 계획을 다시 굴려도 기기 작업 id 가 옛 실패 기록과 겹치지 않게 만든 시각까지 넣는다."""
    return fnv([release_plan, sha256, *(f"{mid}={h}" for mid, h in targets), str(created_at_ms)])


def update_rollout(plan, state, targets, rows, now_ms):
    """(rollout, 까닭) — ready·deferred 인 mac 을 계획 순서로, 조종 기기는 맨 뒤에. 나쵸가 `approval_scope` 를 그대로 사람에게
    보이고 승인하며, 러너(`kasaterm-cli app-update run`)는 작업들로 범위를 다시 지어 대조한다."""
    from tools.release.nacho import scope_hash
    controller = plan["controller"]
    order = [r for r in rows if r["machine_id"] != controller] + [r for r in rows if r["machine_id"] == controller]
    picked, excluded = [], []
    for r in order:
        if r["os"] != "macos":
            excluded.append({"machine_id": r["machine_id"], "code": "not_macos", "why": "mac 만 이 창구로 받는다 — 윈도는 WinSparkle"})
        elif r["status"] == "blocked":
            hard = next((x for x in r["reasons"] if x["code"] not in WAITS), r["reasons"][0])
            excluded.append({"machine_id": r["machine_id"], "code": hard["code"], "why": hard["why"]})
        else:
            picked.append(r)
    probe = []
    for r in picked:
        job, why = update_job(plan, state, targets.get(r["machine_id"]), now_ms, plan_hash="0" * 16)
        if job:
            probe.append((r, job))
        else:
            excluded.append({"machine_id": r["machine_id"], "code": "job_unbuildable", "why": why})
    if not probe:
        return None, "보낼 기기가 없다"
    sha = probe[0][1]["asset"]["sha256"]
    rid = rollout_id(plan["plan_id"], sha, [(j["machine_id"], j["target_hash"]) for _, j in probe], now_ms)
    jobs = [update_job(plan, state, targets.get(r["machine_id"]), now_ms, plan_hash=rid)[0] for r, _ in probe]
    scope = rollout_scope(jobs, controller)
    return {"id": rid, "release_plan": plan["plan_id"], "created_at_ms": now_ms, "approval_scope": scope,
            "approval_scope_hash": scope_hash(scope), "jobs": jobs, "excluded": excluded}, None
