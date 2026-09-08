"""Evidence-backed restart checks. Missing evidence is never promoted to a build."""

import hashlib
import json
import re
import subprocess
from datetime import datetime


GROUPS = {
    "built_not_running": "새 빌드에서 확인할 것",
    "not_built": "아직 빌드되지 않은 것",
    "build_unverified": "새 빌드 파일을 확인할 것",
    "mixed": "일부 수정만 새 빌드에 포함된 것",
    "implementation_unverified": "구현 확인이 필요한 것",
    "carryover": "이전부터 남은 확인",
}


def title(text, limit=100):
    line = " ".join(str(text or "").split()) or "요청 내용 확인"
    return line if len(line) <= limit else line[:limit - 1] + "…"


def requests_in(context):
    rows = context.get("requests_since_start", context.get("requests", context.get("requests_since_run", [])))
    carryover = context.get("carryover", context.get("carryover_requests", []))
    seen = set()
    result = []
    for row in list(rows or []) + list(carryover or []) + list(context.get("timestamp_unknown", [])):
        if not isinstance(row, dict) or not row.get("id") or row["id"] in seen:
            continue
        seen.add(row["id"])
        result.append(row)
    return sorted(result, key=lambda row: (str(row.get("created_at", "")), row["id"]))


def context_hash(context):
    # Prompts are immutable in Store, while updated_at changes for new evidence.
    run = context.get("current_run") or context.get("last_run") or {}
    data = {"run": {key: run.get(key) for key in ("id", "pid", "started_at", "build_id", "component_sha256", "linked_build_id")}, "artifacts": (context.get("latest_observation") or {}).get("evidence", {}).get("artifacts", []), "carried_items": context.get("carryover_items", []),
            "builds": context.get("builds", []), "git_changes": context.get("git_changes", []), "working_changes": context.get("working_changes", []),
            "requests": [(row["id"], row.get("updated_at"), row.get("applied_status"), row.get("reported_status"), row.get("summary")) for row in requests_in(context)]}
    return hashlib.sha256(json.dumps(data, sort_keys=True, ensure_ascii=False, default=str).encode()).hexdigest()


def build_id(build):
    return str(build.get("id") or build.get("build_id") or "")


def matched_builds(row, builds):
    matches = []
    for build in builds:
        linked = build.get("source_request_ids", build.get("request_ids", []))
        if row["id"] in linked:
            matches.append(build)
    return matches


def verified(build):
    return build.get("success") is True and build.get("signature", {}).get("verified") is True


def pet_related(text):
    return any(word in str(text).lower() for word in ("펫", "곽향", "kasapet"))


def conversational_only(text):
    normalized = re.sub(r"[\s.!?…~]+", "", str(text).lower())
    return normalized in {"안녕", "안녕하세요", "여보세요", "hello", "hi", "아지금하고있는겨", "지금하고있는겨", "지금하고있어", "하고있어", "진행중이야", "뭐하고있어", "어디까지했어", "언제끝나"}


def component_checks(checklist, context):
    pet_requests = {row["id"] for row in requests_in(context) if pet_related(row.get("prompt"))}
    for item in checklist["items"]:
        if pet_related(item["title"]) or pet_requests & set(item["source_request_ids"]):
            step = "메인 앱과 펫은 별도 실행입니다. 새 버전의 펫을 켠 뒤 우클릭 메뉴와 동작을 확인합니다."
            if step not in item["steps"]:
                item["steps"].append(step)
            item["component"] = "pet"
    return checklist


def compact_checklist(checklist):
    if checklist.get("supplementary_items") is not None:
        return checklist
    primary = [item for item in checklist.get("items", []) if item["id"].startswith("semantic-")]
    supplementary = [item for item in checklist.get("items", []) if not item["id"].startswith("semantic-")]
    if not primary or not supplementary:
        return checklist
    return dict(checklist, items=primary, supplementary_items=supplementary, supplementary_count=len(supplementary))


def enrich_code_evidence(context, project):
    context = dict(context)
    run = context.get("current_run") or context.get("last_run") or {}
    started = run.get("started_at")
    context.setdefault("git_changes", [])
    context.setdefault("working_changes", [])
    if not started:
        return context
    try:
        datetime.fromisoformat(started.replace("Z", "+00:00"))
        def git(*args):
            result = subprocess.run(["git", "-C", project, *args], capture_output=True, text=True, timeout=12)
            if result.returncode:
                raise ValueError("git evidence unavailable")
            return result.stdout
        observed = (context.get("latest_observation") or {}).get("evidence", {}).get("git", {})
        commits = []
        for commit in observed.get("commits_since_start", []):
            sha = commit.get("commit", "")
            if re.fullmatch(r"[a-f0-9]{40}", sha):
                commits.append({"id": "commit:" + sha, "commit": sha, "title": commit.get("subject", ""), "created_at": commit.get("committed_at"), "build_ids": []})
        for build in context.get("builds", []):
            source = build.get("source", {})
            head = source.get("source_commit") or source.get("observed_head")
            if not verified(build) or not isinstance(head, str) or not re.fullmatch(r"[a-f0-9]{40}", head):
                continue
            ancestors = set(git("rev-list", f"--since={started}", head, "--").splitlines())
            for commit in commits:
                if commit["commit"] in ancestors:
                    commit["build_ids"].append(build_id(build))
                    commit["source_exact"] = source.get("source_commit") == head
        context["git_changes"] = commits
        context["working_changes"] = ["working_tree_changes"] if observed.get("dirty") else []
    except (OSError, ValueError, subprocess.TimeoutExpired):
        context["git_observation_error"] = "git_evidence_unavailable"
    return context


def make_checklist(context):
    rows = requests_in(context)
    carries = {row.get("id") for row in context.get("carryover", context.get("carryover_requests", [])) if isinstance(row, dict)}
    unknown_time = {row.get("id") for row in context.get("timestamp_unknown", []) if isinstance(row, dict)}
    builds = context.get("builds", [])
    items = []
    run = context.get("current_run") or context.get("last_run") or {}
    artifacts = (context.get("latest_observation") or {}).get("evidence", {}).get("artifacts", [])
    current_ready = {item.get("build_id") for item in artifacts if item.get("status") == "verified_ready"}
    available = {build_id(build) for build in builds if verified(build) and build_id(build) in current_ready and build_id(build) != run.get("linked_build_id") and str(build.get("completed_at", "")) > str(run.get("started_at", ""))}
    pet_available = {build_id(build) for build in builds if verified(build) and build_id(build) in current_ready and str(build.get("completed_at", "")) > str(run.get("started_at", ""))}
    for change in context.get("git_changes", []):
        ready = set(change.get("build_ids", [])) & (pet_available if pet_related(change.get("title")) else available)
        state = "built_not_running" if ready else "build_unverified" if change.get("build_ids") else "not_built"
        items.append({"id": change["id"], "title": title(change.get("title")), "steps": ["앱을 다시 켠 뒤 이 변경에 해당하는 화면을 열어 봅니다." if ready else "이 수정이 포함된 빌드 기록과 현재 앱 파일이 일치하는지 먼저 확인합니다.", "요청한 동작과 기존 동작이 모두 되는지 눈으로 확인합니다."], "source_request_ids": [], "buildstate": state, "evidence_ids": [change["id"]] + sorted(ready), "context_notes": ["실제 코드 변경 근거입니다. 기능 동작은 사용자 확인 전입니다."], "uncertain": not bool(ready and change.get("source_exact")), "basis": "code_change"})
    if context.get("working_changes"):
        evidence = "worktree:" + hashlib.sha256("\n".join(context["working_changes"]).encode()).hexdigest()[:20]
        items.append({"id": evidence, "title": "아직 작업 중인 수정", "steps": ["작업 중인 수정이 완료되고 새 빌드에 포함됐는지 확인합니다."], "source_request_ids": [], "buildstate": "not_built", "evidence_ids": [evidence], "context_notes": ["아직 커밋되지 않은 변경이 관측됐습니다."], "uncertain": True, "basis": "code_change"})
    for old in context.get("carryover_items", []):
        if old.get("id") and not any(item["id"] == old["id"] for item in items):
            items.append(dict(old, buildstate="carryover"))
    previous_by_source = {}
    context_only = []
    for row in rows:
        source = row.get("source_id") or row.get("session_id") or row["id"]
        prior = previous_by_source.get(source)
        if conversational_only(row.get("prompt")):
            context_only.append(row["id"])
            continue
        if row.get("applied_status") in ("applied", "not_applicable"):
            previous_by_source[source] = (row, None)
            continue
        text = str(row.get("prompt") or "")
        short_reply = "".join(text.split()).rstrip(".!?…") in {"ㄱ", "ㄱㄱ", "ㅇㅇ", "응", "네", "좋아", "오케이", "진행해", "해줘", "우클릭메뉴에"}
        if short_reply and not prior:
            context_only.append(row["id"])
            continue
        if short_reply and prior and row["id"] not in unknown_time and prior[0]["id"] not in unknown_time:
            if prior[1] is not None:
                items[prior[1]]["source_request_ids"].append(row["id"])
                items[prior[1]]["context_notes"].append(title(text, 40))
            else:
                context_only.append(row["id"])
            previous_by_source[source] = (row, prior[1])
            continue
        linked = matched_builds(row, builds)
        built = [build for build in linked if verified(build) and build_id(build) in (pet_available if pet_related(text) else available)]
        evidence = [build_id(build) for build in linked if build_id(build)]
        if row["id"] in unknown_time:
            state = "implementation_unverified"
        elif row["id"] in carries:
            state = "carryover"
        elif built:
            state = "built_not_running"
        elif linked and not any(verified(build) for build in linked):
            state = "not_built"
        else:
            state = "implementation_unverified"
        summary = title(text)
        steps = (["새로 만든 앱에 이 변경이 포함됐는지 확인합니다.", f"화면에서 ‘{summary}’ 요청한 동작을 직접 확인합니다."] if built else
                 ["요청에 대응하는 실제 수정·빌드 근거를 먼저 확인합니다.", "근거 확인 전에는 재시작으로 바뀐다고 단정하지 않습니다."])
        items.append({"id": "check-" + row["id"], "title": summary, "steps": steps,
                      "source_request_ids": [row["id"]], "buildstate": state,
                      "evidence_ids": evidence, "context_notes": ["요청 시점을 알 수 없어 이번 앱 실행 이후라고 단정할 수 없습니다."] if row["id"] in unknown_time else [], "uncertain": not bool(built)})
        previous_by_source[source] = (row, len(items) - 1)
    return {"items": items, "groups": GROUPS, "coverage": {"total_requests": len(rows), "covered_request_ids": [row["id"] for row in rows], "semantic_request_ids": [], "context_only_request_ids": context_only, "missing_request_ids": []},
            "context": {"current_run": context.get("current_run"), "last_run": context.get("last_run"), "previous_run": context.get("previous_run"), "build_count": len(builds), "ready_build_count": len(current_ready), "artifact_states": [item.get("status") for item in artifacts], "since_start_count": len(context.get("requests_since_start", [])), "carryover_count": len(carries), "timestamp_unknown_count": len(unknown_time)}, "context_hash": context_hash(context)}


def apply_semantic(checklist, proposals, known_requests, known_builds, context_only=None):
    """A model can improve wording; only validated IDs may enter an item."""
    accepted = []
    covered = set()
    covered_code = set()
    seen_proposals = set()
    context_only = set(checklist["coverage"].get("context_only_request_ids", [])) | (set(context_only or []) & known_requests)
    for item in proposals:
        if not isinstance(item, dict):
            continue
        ids = item.get("source_request_ids", [])
        evidence = item.get("evidence_ids", [])
        if not isinstance(ids, list) or not all(isinstance(key, str) and key in known_requests for key in ids):
            continue
        if not isinstance(evidence, list) or not all(isinstance(key, str) and key in known_builds for key in evidence):
            continue
        steps = item.get("steps", [])
        if not isinstance(steps, list) or not all(isinstance(step, str) for step in steps):
            continue
        fingerprint = (title(item.get("title")), tuple(sorted(ids)), tuple(sorted(evidence)), tuple(steps))
        if fingerprint in seen_proposals:
            continue
        seen_proposals.add(fingerprint)
        code_ids = {key for key in evidence if key.startswith(("commit:", "worktree:"))}
        originals = [old for old in checklist["items"] if set(ids) & set(old["source_request_ids"]) or code_ids & set(old["evidence_ids"])]
        if not originals:
            continue
        # Semantic associations alone cannot prove that a feature entered a binary.
        states = {old["buildstate"] for old in originals}
        ready_code = any(old.get("basis") == "code_change" and old["buildstate"] == "built_not_running" for old in originals)
        state = next(iter(states)) if len(states) == 1 else "mixed" if ready_code and states & {"not_built", "build_unverified"} else "built_not_running" if ready_code else "implementation_unverified"
        stable = json.dumps({"requests": sorted(ids), "evidence": sorted(set(key for old in originals for key in old["evidence_ids"]))}, sort_keys=True)
        accepted.append({"id": "semantic-" + hashlib.sha256(stable.encode()).hexdigest()[:20], "title": title(item.get("title")), "steps": [title(step, 240) for step in steps[:5]],
                         "source_request_ids": ids, "evidence_ids": sorted(set(key for old in originals for key in old["evidence_ids"])), "buildstate": state, "uncertain": any(old["uncertain"] for old in originals), "context_notes": ["새 빌드에 든 부분과 아직 포함 여부를 확인해야 하는 수정이 함께 있습니다."] if state == "mixed" else []})
        covered.update(ids)
        covered_code.update(old["id"] for old in originals if not old["source_request_ids"])
    for item in checklist["items"]:
        remaining = [key for key in item["source_request_ids"] if key not in covered and key not in context_only]
        if remaining or (not item["source_request_ids"] and item["id"] not in covered_code):
            accepted.append(dict(item, source_request_ids=remaining))
    checklist["items"] = accepted
    checklist["coverage"]["semantic_request_ids"] = sorted(covered)
    checklist["coverage"]["context_only_request_ids"] = sorted(context_only - covered)
    return checklist
