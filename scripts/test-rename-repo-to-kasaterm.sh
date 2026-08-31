#!/usr/bin/env bash
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd -P)
FIXTURE=$(mktemp -d "${TMPDIR:-/tmp}/kasaterm-rename-test.XXXXXX")
FIXTURE=$(cd "$FIXTURE" && pwd -P)
trap 'rm -rf "$FIXTURE"' EXIT

PARENT="$FIXTURE/work"
OLD="$PARENT/tmuxify"
NEW="$PARENT/kasaterm"
LINKED="$PARENT/linked"
FAKE_HOME="$FIXTURE/home"
mkdir -p "$OLD/scripts" "$OLD/app/kasaterm" "$FAKE_HOME"
cp "$HERE/rename-repo-to-kasaterm.sh" "$OLD/scripts/"
cp "$HERE/migrate-repo-path.py" "$OLD/scripts/"
touch "$OLD/Cargo.toml"

git -C "$OLD" init -q
git -C "$OLD" config user.name test
git -C "$OLD" config user.email test@example.com
git -C "$OLD" add .
git -C "$OLD" commit -qm initial
git -C "$OLD" worktree add -qb linked "$LINKED"

read -r OLD_SLUG NEW_SLUG OLD_TEAM NEW_TEAM < <(
  python3 - "$HERE/migrate-repo-path.py" "$OLD" "$NEW" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("migration", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(module.project_slug(sys.argv[2]), module.project_slug(sys.argv[3]), module.team_name(sys.argv[2]), module.team_name(sys.argv[3]))
PY
)

mkdir -p \
  "$FAKE_HOME/.codex" \
  "$FAKE_HOME/.claude/plugins" \
  "$FAKE_HOME/.claude/projects/$OLD_SLUG" \
  "$FAKE_HOME/.claude/projects/$NEW_SLUG" \
  "$FAKE_HOME/.claude/projects/$OLD_SLUG-sibling" \
  "$FAKE_HOME/.claude/tasks/$OLD_TEAM" \
  "$FAKE_HOME/.claude/tasks/$NEW_TEAM" \
  "$FAKE_HOME/.claude/teams/$OLD_TEAM" \
  "$FAKE_HOME/.claude/sessions" \
  "$FAKE_HOME/dotfiles" \
  "$FAKE_HOME/.config/kasaterm/agent-roster"

printf '[mcp_servers.browser]\nargs = ["%s/kasachrome/mcp/server.mjs"]\n\n[projects."%s"]\ntrust_level = "trusted"\n' \
  "$OLD" "$OLD" > "$FAKE_HOME/.codex/config.toml"
printf '{"hooks":[{"path":"%s"}]}\n' "$OLD" > "$FAKE_HOME/dotfiles/settings.json"
ln -s "$FAKE_HOME/dotfiles/settings.json" "$FAKE_HOME/.claude/settings.json"
printf '{"projects":{"%s":{"oldOnly":true},"%s":{"newOnly":true}},"mcp":"%s/kasachrome/mcp/server.mjs"}\n' \
  "$OLD" "$NEW" "$OLD" > "$FAKE_HOME/.claude.json"
printf '{"kasaterm":{"source":{"path":"%s"},"installLocation":"%s"}}\n' \
  "$OLD" "$OLD" > "$FAKE_HOME/.claude/plugins/known_marketplaces.json"
printf '{"cwd":"%s","windows":[{"cwd":"%s/sub"}]}\n' "$OLD" "$OLD" > "$FAKE_HOME/.config/kasaterm/session.json"
printf '{"w":100,"h":100}\n' > "$FAKE_HOME/.config/kasaterm/window.json"
printf '{"panes":{"%%1":{"cwd":"%s"}}}\n' "$OLD" > "$FAKE_HOME/.config/kasaterm/daemon.sock.state"
printf '{"%%1":{"cwd":"%s","ts":2}}\n' "$OLD" > "$FAKE_HOME/.config/kasaterm/agent-roster/$OLD_SLUG.json"
printf '{"%%1":{"cwd":"%s","ts":1},"%%2":{"cwd":"%s","ts":1}}\n' \
  "$NEW" "$NEW" > "$FAKE_HOME/.config/kasaterm/agent-roster/$NEW_SLUG.json"
printf '{"cwd":"%s"}\n' "$OLD" > "$FAKE_HOME/.claude/tasks/$OLD_TEAM/1.json"
printf '{"cwd":"%s","new":true}\n' "$NEW" > "$FAKE_HOME/.claude/tasks/$NEW_TEAM/2.json"
printf '{"members":[{"cwd":"%s"}]}\n' "$OLD" > "$FAKE_HOME/.claude/teams/$OLD_TEAM/config.json"
printf '{"pid":999999,"cwd":"%s"}\n' "$OLD" > "$FAKE_HOME/.claude/sessions/999999.json"
printf '{"cwd":"%s"}\n' "$OLD" > "$FAKE_HOME/.claude/projects/$OLD_SLUG/old.jsonl"
printf '{"cwd":"%s"}\n' "$NEW" > "$FAKE_HOME/.claude/projects/$NEW_SLUG/new.jsonl"
printf '{"cwd":"%s-sibling"}\n' "$OLD" > "$FAKE_HOME/.claude/projects/$OLD_SLUG-sibling/sibling.jsonl"

DRY_BACKUP="$FIXTURE/dry-backup"
(cd "$PARENT" && "$OLD/scripts/rename-repo-to-kasaterm.sh" \
  --source "$OLD" --target "$NEW" --home "$FAKE_HOME" --backup-dir "$DRY_BACKUP")
[[ -d "$OLD" && ! -e "$NEW" && ! -e "$DRY_BACKUP" ]]
grep -Fq "$OLD" "$FAKE_HOME/.codex/config.toml"

BACKUP="$FIXTURE/backup"
(cd "$PARENT" && "$OLD/scripts/rename-repo-to-kasaterm.sh" \
  --source "$OLD" --target "$NEW" --home "$FAKE_HOME" --backup-dir "$BACKUP" --apply)

[[ -d "$NEW" && ! -e "$OLD" && ! -L "$OLD" ]]
[[ $(git -C "$NEW" rev-parse --show-toplevel) = "$NEW" ]]
[[ $(git -C "$LINKED" rev-parse --show-toplevel) = "$LINKED" ]]
grep -Fq "$NEW/.git/worktrees/linked" "$LINKED/.git"
[[ -d "$FAKE_HOME/.claude/projects/$NEW_SLUG" && ! -e "$FAKE_HOME/.claude/projects/$OLD_SLUG" ]]
[[ -f "$FAKE_HOME/.claude/projects/$NEW_SLUG/old.jsonl" && -f "$FAKE_HOME/.claude/projects/$NEW_SLUG/new.jsonl" ]]
[[ -f "$FAKE_HOME/.claude/projects/$OLD_SLUG-sibling/sibling.jsonl" ]]
[[ -d "$FAKE_HOME/.claude/tasks/$NEW_TEAM" && ! -e "$FAKE_HOME/.claude/tasks/$OLD_TEAM" ]]
[[ -d "$FAKE_HOME/.claude/teams/$NEW_TEAM" && ! -e "$FAKE_HOME/.claude/teams/$OLD_TEAM" ]]
[[ -f "$FAKE_HOME/.config/kasaterm/agent-roster/$NEW_SLUG.json" && ! -e "$FAKE_HOME/.config/kasaterm/agent-roster/$OLD_SLUG.json" ]]
[[ -f "$BACKUP/state-migration-manifest.json" && -f "$BACKUP/worktrees-before.txt" ]]
[[ -L "$FAKE_HOME/.claude/settings.json" ]]

python3 - "$FAKE_HOME" "$NEW" <<'PY'
import json, pathlib, sys
home, new = pathlib.Path(sys.argv[1]), sys.argv[2]
claude = json.loads((home / ".claude.json").read_text())
assert claude["projects"][new] == {"oldOnly": True, "newOnly": True}
roster = json.loads(next((home / ".config/kasaterm/agent-roster").glob("*.json")).read_text())
assert roster["%1"]["ts"] == 2 and roster["%1"]["cwd"] == new
assert "%2" in roster
PY

if rg -l --fixed-strings "$OLD" \
  "$FAKE_HOME/.codex/config.toml" \
  "$FAKE_HOME/.claude/settings.json" \
  "$FAKE_HOME/.claude.json" \
  "$FAKE_HOME/.claude/plugins/known_marketplaces.json" \
  "$FAKE_HOME/.claude/tasks" \
  "$FAKE_HOME/.claude/teams" \
  "$FAKE_HOME/.claude/sessions" \
  "$FAKE_HOME/.config/kasaterm" >/dev/null; then
  printf '이전 경로가 남았습니다\n' >&2
  exit 1
fi

SECOND_BACKUP="$FIXTURE/backup-second"
(cd "$PARENT" && "$NEW/scripts/rename-repo-to-kasaterm.sh" \
  --source "$OLD" --target "$NEW" --home "$FAKE_HOME" --backup-dir "$SECOND_BACKUP" --apply)
[[ -f "$SECOND_BACKUP/state-migration-manifest.json" ]]

printf 'rename migration fixture: OK\n'
