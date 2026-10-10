#!/usr/bin/env python3
"""워크스페이스 크레이트 경계 검사. 규칙과 이유는 docs/workspace.md.

    python3 scripts/check-workspace.py             # 위반이 있으면 나열하고 1로 끝난다
    python3 scripts/check-workspace.py --dump      # 패키지별 의존 의미를 JSON 으로(전후 비교용)
    python3 scripts/check-workspace.py --self-test # 위반을 심어 검사가 잡는지 확인

`cargo metadata --no-deps` 와 각 Cargo.toml 만 읽는다. 내려받기·빌드는 하지 않는다.
"""

import argparse
import copy
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 크레이트 → (층, 순위). 의존은 순위가 더 작은 크레이트로만 흐른다.
# 새 크레이트는 여기와 docs/workspace.md 의 표에 함께 올린다.
LAYERS = {
    "kasa-screen": ("core", 0),
    "kasa-ime": ("core", 0),
    "kasa-cells": ("core", 0),
    "kasa-pty": ("core", 1),
    "kasa-bridge": ("core", 1),
    "kasa-gridview": ("core", 1),
    "kasa-socket": ("protocol", 2),
    "kasa-net": ("transport", 2),
    "kasa-net-ffi": ("transport", 3),
    "kasa-agents": ("collab", 3),
    "kasa-collab": ("collab", 4),
    "kasa-mcp": ("host", 5),
    "kasa-pet-config": ("app", 6),
    "kasaterm": ("app", 7),
    "kasapet": ("app", 7),
}
SPIKE_DIR = "spikes/"
HOST_RANK = LAYERS["kasa-mcp"][1]

# 받는 쪽이 없어야 하는 산출물(실행 파일·폰 정적 라이브러리).
LEAVES = {"kasaterm", "kasapet", "kasa-net-ffi"}

# 옛 tmux 백엔드. 화면 낱말은 kasa-screen 에서 받는다 — 새로 기대는 크레이트를 만들지 않는다.
LEGACY_DEPENDENTS = {"kasa-bridge": {"kasaterm"}}

# 창·GPU 를 만질 수 있는 크레이트. 나머지 제품 크레이트는 GUI 없이 돈다(서버·관문·`kasa tui`).
GUI_CRATES = {"kasa-cells", "kasa-gridview", "kasaterm", "kasapet"}
GUI_DEPS = {
    "winit", "wgpu", "wgpu-hal", "wry", "tao", "muda", "raw-window-handle", "softbuffer",
    "objc2-app-kit", "objc2-web-kit", "eframe", "egui", "iced", "kasa-cells", "kasa-gridview",
}

# 비동기 런타임·HTTP·P2P. 핵심·프로토콜 층에는 없고, 협업 층에는 feature 뒤(optional)로만 있다.
NET_DEPS = {"tokio", "reqwest", "axum", "hyper", "tower", "tokio-tungstenite", "iroh", "quinn"}

# 기본 기능이 무거운 내부 크레이트 → 그 기본 기능. 호스트보다 아래 층은 꺼서 받는다.
HEAVY_DEFAULTS = {"kasa-socket": {"app-update"}, "kasa-pty": {"os-clipboard"}}

# 일부러 기본 기능을 끈 외부 크레이트. 켜면 쓰지 않는 코덱·TLS·파서가 실린다.
NO_DEFAULT_FEATURES = {"reqwest", "image", "zip", "similar", "pulldown-cmark", "objc2-intents"}

# crates.io 에 낼 수 있게 둔 크레이트. 경로 의존이 끼면 낼 수 없다.
PUBLIC = {"kasa-cells": ("description", "license", "repository")}

DEP_TABLES = ("dependencies", "dev-dependencies", "dev_dependencies", "build-dependencies", "build_dependencies")


def cargo_metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8",
    )
    return json.loads(out.stdout)


def rel(path, root):
    return str(Path(path).resolve().relative_to(Path(root).resolve()))


def is_spike(pkg, root):
    return rel(pkg["manifest_path"], root).startswith(SPIKE_DIR)


def manifest_deps(doc):
    """Cargo.toml 한 장의 (표 이름, 의존 이름, 값) 전부 — target 표 포함."""
    for table in DEP_TABLES:
        for name, spec in doc.get(table, {}).items():
            yield table, name, spec
    for cfg, tdoc in doc.get("target", {}).items():
        for table in DEP_TABLES:
            for name, spec in tdoc.get(table, {}).items():
                yield f"target.{cfg}.{table}", name, spec


def check_metadata(meta):
    errors = []
    root = meta["workspace_root"]
    packages = {p["name"]: p for p in meta["packages"]}
    default_ids = set(meta.get("workspace_default_members") or [])
    spikes = {n for n, p in packages.items() if is_spike(p, root)}

    for name, pkg in packages.items():
        if name in spikes:
            if pkg["id"] in default_ids:
                errors.append(f"{name}: 실험 크레이트가 default-members 에 있다")
            continue
        if name not in LAYERS:
            errors.append(f"{name}: 층이 정해지지 않았다 — scripts/check-workspace.py LAYERS 와 docs/workspace.md 에 올려라")
        if pkg["id"] not in default_ids:
            errors.append(f"{name}: 제품 크레이트가 default-members 에 없다")
    if not default_ids:
        errors.append("루트 Cargo.toml 에 default-members 가 없다")
    for missing in sorted(set(LAYERS) - set(packages)):
        errors.append(f"{missing}: LAYERS 에 있는데 워크스페이스에 없다 — 표를 고쳐라")

    for name, pkg in packages.items():
        spike = name in spikes
        layer, rank = LAYERS.get(name, ("spike", 99))
        for d in pkg["dependencies"]:
            dep, kind = d["name"], d["kind"] or "normal"
            where = f"{name} → {dep}" + ("" if kind == "normal" else f" ({kind})")
            internal = dep in packages
            if internal and d["rename"]:
                errors.append(f"{where}: 내부 크레이트를 다른 이름({d['rename']})으로 받는다 — 경로가 감춰진다")
            if internal and dep in spikes:
                errors.append(f"{where}: 실험 크레이트에 기댄다")
                continue
            if internal and dep in LEAVES:
                errors.append(f"{where}: {dep} 는 산출물이라 받는 쪽이 없어야 한다")
            if internal and not spike and dep in LAYERS:
                dlayer, drank = LAYERS[dep]
                if drank >= rank:
                    errors.append(f"{where}: 경계 역전 — {layer}({rank}) 가 {dlayer}({drank}) 에 기댄다")
            if internal and dep in LEGACY_DEPENDENTS and not spike and name not in LEGACY_DEPENDENTS[dep]:
                errors.append(f"{where}: 옛 tmux 백엔드에 새로 기댄다 — 화면 낱말은 kasa-screen 에서 받아라")
            if spike:
                continue
            if internal and dep in HEAVY_DEFAULTS and rank < HOST_RANK:
                leaked = HEAVY_DEFAULTS[dep] & set(d["features"])
                if d["uses_default_features"] or leaked:
                    what = "기본 기능" if d["uses_default_features"] else ", ".join(sorted(leaked))
                    errors.append(f"{where}: 가벼운 소비자에 {what} 이 샌다 — default-features = false 로 받아라")
            if dep in NO_DEFAULT_FEATURES and d["uses_default_features"]:
                errors.append(f"{where}: 기본 기능을 켰다 — 쓰는 기능만 골라 받아라")
            if kind == "dev":
                continue
            if dep in GUI_DEPS and name not in GUI_CRATES:
                errors.append(f"{where}: GUI 없는 크레이트가 창·GPU 의존을 들였다")
            if dep in NET_DEPS and layer in ("core", "protocol"):
                errors.append(f"{where}: {layer} 층에 네트워크 스택이 들어왔다")
            if dep in NET_DEPS and layer == "collab" and not d["optional"]:
                errors.append(f"{where}: 협업 층의 네트워크 스택은 feature 뒤(optional)에 둔다")

        if name in PUBLIC:
            for field in PUBLIC[name]:
                if not pkg.get(field):
                    errors.append(f"{name}: 공개 크레이트에 {field} 가 없다")
            for d in pkg["dependencies"]:
                if d["name"] in packages and d["kind"] != "dev":
                    errors.append(f"{name} → {d['name']}: 공개 크레이트가 경로 의존을 들였다")
    return errors


def check_manifests(meta):
    errors = []
    root = Path(meta["workspace_root"])
    root_text = (root / "Cargo.toml").read_text(encoding="utf-8")
    root_doc = tomllib.loads(root_text)
    ws = root_doc["workspace"]
    ws_deps = ws.get("dependencies", {})
    packages = {p["name"]: p for p in meta["packages"]}

    first = re.search(r'^version = "([^"]*)"', root_text, re.M)
    if not first or first.group(1) != ws.get("package", {}).get("version"):
        errors.append("루트 Cargo.toml: 첫 `version = ` 줄이 [workspace.package] 버전이 아니다 — 릴리스 도구가 그 줄을 고친다")

    for dep in list(HEAVY_DEFAULTS) + sorted(NO_DEFAULT_FEATURES & set(ws_deps)):
        spec = ws_deps.get(dep)
        if isinstance(spec, dict) and spec.get("default-features", spec.get("default_features", True)):
            errors.append(f"[workspace.dependencies] {dep}: default-features = false 로 둔다 — 받는 쪽이 끌 수 없다")
        elif isinstance(spec, str):
            errors.append(f"[workspace.dependencies] {dep}: default-features = false 로 둔다 — 받는 쪽이 끌 수 없다")

    used = set()
    declared = {}
    for name, pkg in packages.items():
        path = Path(pkg["manifest_path"])
        doc = tomllib.loads(path.read_text(encoding="utf-8"))
        spike = rel(path, root).startswith(SPIKE_DIR)
        for table, dep, spec in manifest_deps(doc):
            real = spec.get("package", dep) if isinstance(spec, dict) else dep
            inherited = isinstance(spec, dict) and spec.get("workspace") is True
            if inherited:
                used.add(dep)
            where = f"{rel(path, root)} [{table}] {dep}"
            if real in packages and not inherited:
                errors.append(f"{where}: 내부 크레이트는 `workspace = true` 로 받는다(경로를 루트 한 곳에)")
            if spike:
                continue
            if dep in ws_deps and not inherited:
                errors.append(f"{where}: [workspace.dependencies] 에 있는 판을 따로 적었다 — `workspace = true` 로")
            if real not in packages and not inherited:
                declared.setdefault(real, set()).add(name)
    for dep, owners in sorted(declared.items()):
        if len(owners) > 1:
            errors.append(f"{dep}: 제품 크레이트 {', '.join(sorted(owners))} 가 판을 따로 적었다 — [workspace.dependencies] 로 모아라")
    for dep in sorted(set(ws_deps) - used):
        errors.append(f"[workspace.dependencies] {dep}: 아무 크레이트도 받지 않는다")
    return errors


def dump(meta):
    root = meta["workspace_root"]
    out = {}
    for p in meta["packages"]:
        deps = [
            {
                "name": d["name"], "req": d["req"], "kind": d["kind"], "target": d["target"],
                "rename": d["rename"], "optional": d["optional"],
                "default_features": d["uses_default_features"], "features": sorted(set(d["features"])),
                "source": d["source"], "path": rel(d["path"], root) if d.get("path") else None,
            }
            for d in p["dependencies"]
        ]
        out[p["name"]] = {
            "version": p["version"], "edition": p["edition"], "license": p["license"],
            "publish": p["publish"], "description": p["description"], "repository": p["repository"],
            "features": {k: sorted(v) for k, v in p["features"].items()},
            "targets": sorted(f"{','.join(t['kind'])}:{t['name']}" for t in p["targets"]),
            "dependencies": sorted(deps, key=lambda d: json.dumps(d, sort_keys=True)),
        }
    out["<workspace>"] = {
        "default_members": sorted(i.split("#")[0].rsplit("/", 1)[-1] for i in meta.get("workspace_default_members") or []),
    }
    return out


def self_test(meta):
    """위반 하나씩 심어 각 규칙이 잡는지 본다. 지금 트리는 통과해야 한다."""
    base = check_metadata(meta)
    assert not base, f"지금 트리가 이미 위반이다: {base}"

    def inject(pkg_name, **dep):
        m = copy.deepcopy(meta)
        pkg = next(p for p in m["packages"] if p["name"] == pkg_name)
        d = {"name": None, "req": "*", "kind": None, "target": None, "rename": None, "optional": False,
             "uses_default_features": True, "features": [], "source": None}
        d.update(dep)
        pkg["dependencies"].append(d)
        return check_metadata(m)

    cases = {
        "경계 역전": inject("kasa-pty", name="kasa-socket", uses_default_features=False),
        "옛 tmux 백엔드": inject("kasa-mcp", name="kasa-bridge"),
        "기본 기능": inject("kasa-agents", name="kasa-pty"),
        "app-update": inject("kasa-collab", name="kasa-socket", uses_default_features=False, features=["app-update"]),
        "창·GPU": inject("kasa-socket", name="winit", req="^0.30", source="registry+https://github.com/rust-lang/crates.io-index"),
        "네트워크 스택": inject("kasa-screen", name="tokio", req="^1.52.3", source="registry+https://github.com/rust-lang/crates.io-index"),
        "optional": inject("kasa-collab", name="reqwest", req="^0.12", uses_default_features=False, source="registry+https://github.com/rust-lang/crates.io-index"),
        "실험 크레이트": inject("kasaterm", name="iced-term"),
        "산출물": inject("kasa-mcp", name="kasaterm"),
        "다른 이름": inject("kasa-mcp", name="kasa-screen", rename="kasa_bridge"),
        "공개 크레이트": inject("kasa-cells", name="kasa-screen"),
    }
    failed = [k for k, errs in cases.items() if not any(k in e for e in errs)]
    m = copy.deepcopy(meta)
    m["workspace_default_members"] = [i for i in m.get("workspace_default_members") or [] if "kasa-mcp" not in i]
    if not any("default-members 에 없다" in e for e in check_metadata(m)):
        failed.append("default-members")
    if failed:
        print("검사가 못 잡은 위반: " + ", ".join(failed), file=sys.stderr)
        return 1
    print(f"self-test 통과: 심은 위반 {len(cases) + 1}종을 모두 잡았다")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dump", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    meta = cargo_metadata()
    if args.dump:
        json.dump(dump(meta), sys.stdout, indent=1, sort_keys=True, ensure_ascii=False)
        print()
        return 0
    if args.self_test:
        return self_test(meta)
    errors = check_metadata(meta) + check_manifests(meta)
    for e in errors:
        print(f"  - {e}", file=sys.stderr)
    if errors:
        print(f"워크스페이스 경계 위반 {len(errors)}건 — docs/workspace.md", file=sys.stderr)
        return 1
    n = len(meta["packages"])
    print(f"워크스페이스 경계 통과: 크레이트 {n}개")
    return 0


if __name__ == "__main__":
    # 보고는 한글이다. Windows 의 파이프·콘솔은 로캘 코드 페이지(cp1252 등)라 그대로 쓰면 인코딩에서 죽는다.
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    sys.exit(main())
