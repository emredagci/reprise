#!/usr/bin/env bash
# Runs as `dev` inside the systemd container (`.github/linux-check.sh`): connects a stand-in Claude Code, has
# systemd schedule the sweep, reports a session as ended and checks that the scheduled job distills it into
# the vault, then removes everything again with `sbrain uninstall`. Fails on the first thing that is not as it should be.
set -euo pipefail

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
# `has <text> <command…>`: the command's output holds the text. Not a pipe into grep -q: grep stopping
# early would fail the command under pipefail.
has() { local text="$1"; shift; grep -qF -- "$text" <<< "$("$@")"; }
journal() { journalctl --user -u sbrain-sweep.service --no-pager; }

answer="$1"
session=1a2b3c4d-0000-4000-8000-000000000000

# A stand-in `claude` where Claude Code's own installer puts it, outside the systemd job's PATH.
mkdir -p ~/.local/bin ~/vault ~/work/app
cat > ~/.local/bin/claude <<SH
#!/bin/sh
if [ "\$1" = --version ]; then echo "2.1.0 (Claude Code)"; exit 0; fi
cat > /dev/null
printf '%s' '$answer'
SH
chmod 755 ~/.local/bin/claude
git -C ~/work/app init -q
git -C ~/work/app remote add origin git@github.com:acme/app.git

sbrain setup set --vault ~/vault --auto > /dev/null
sbrain connect claude --yes > /dev/null
[ "$(grep -c 'sbrain hook' ~/.claude/settings.json)" = 4 ] || fail "connect did not add the four hooks"
ok "connect added the hooks"
start() { echo "{\"session_id\":\"s0\",\"cwd\":\"$HOME/work/app\"}" | sbrain hook session-start; }
has 'project: app' start || fail "session start gave no project context"
ok "session start gives the project's context"

sbrain sweep schedule --every 1 > /dev/null
systemctl --user is-enabled sbrain-sweep.timer > /dev/null || fail "the timer is not enabled"
has sbrain-sweep.timer systemctl --user list-timers --no-pager || fail "the timer is not listed"
ok "the sweep is scheduled with systemd"

# The first run only marks where to start.
systemctl --user start sbrain-sweep.service
has 'First run' journal || fail "the first run did not start"
sleep 1.2

dir=~/.claude/projects/-home-dev-work-app
mkdir -p "$dir"
for i in 0 1 2; do
  echo "{\"type\":\"user\",\"cwd\":\"$HOME/work/app\",\"timestamp\":\"2026-10-26T09:0$i:00Z\",\"message\":{\"content\":\"message $i about caching the login query\"}}"
done > "$dir/$session.jsonl"
echo "{\"session_id\":\"$session\",\"cwd\":\"$HOME/work/app\"}" | sbrain hook session-end

systemctl --user start sbrain-sweep.service
[ "$(systemctl --user show sbrain-sweep.service -p Result --value)" = success ] || fail "the job failed"
has 'ok 1a2b3c4d [app]' journal || { journal | tail -20; fail "the job did not distill the session"; }
ls ~/vault/projects/app/sessions/*-1a2b3c4d.md > /dev/null || fail "no card was written"
has 'sweep(app): 1a2b3c4d' git -C ~/vault log --oneline || fail "the vault was not committed"
ok "the scheduled job found claude in ~/.local/bin, distilled the session and committed it"

has 'scheduled (systemd)' sbrain status || fail "status does not show the schedule"
has 'hooks run /usr/local/bin/sbrain (this sbrain)' sbrain status || fail "status does not show the hooks"
ok "status shows the schedule and the hooks"

has 'unschedule the sweep' sbrain uninstall || fail "uninstall does not list the scheduled sweep"
sbrain uninstall --yes > /dev/null
[ ! -e ~/.config/systemd/user/sbrain-sweep.timer ] || fail "uninstall left the timer"
! has sbrain-sweep systemctl --user list-timers --all --no-pager || fail "the timer is still loaded"
! grep -q sbrain ~/.claude/settings.json || fail "uninstall left hooks behind"
[ ! -e ~/.config/sbrain ] || fail "uninstall left sbrain's settings"
[ -f ~/vault/.sbrain/vault.toml ] || fail "uninstall touched the vault"
ok "uninstall removes the timer, the hooks and the settings, and keeps the vault"
echo "Linux scenario passed."
