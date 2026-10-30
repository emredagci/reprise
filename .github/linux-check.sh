#!/usr/bin/env bash
# Checks reprise on Linux in Docker: in Ubuntu 24.04, the CI steps (format, lints with
# warnings as errors, every test) and a release build; then, in a container booting systemd, the scheduled
# sweep end to end (`.github/linux/scenario.sh`). Builds for the Docker host's architecture.
#
#   ./.github/linux-check.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
here=.github/linux
container=reprise-linux-systemd
volume=reprise-linux-target

docker build -q -t reprise-linux-check -f "$here/check.Dockerfile" "$here" > /dev/null 2>&1
docker build -q -t reprise-linux-systemd -f "$here/systemd.Dockerfile" "$here" > /dev/null 2>&1
docker volume create "$volume" > /dev/null

echo "== CI steps on $(docker info --format '{{.OSType}}/{{.Architecture}}')"
docker run --rm -v "$root":/src:ro -v "$volume":/home/dev/target -e CARGO_TARGET_DIR=/home/dev/target \
  -e RUSTFLAGS="-D warnings" reprise-linux-check bash -euo pipefail -c '
    rsync -a --delete --exclude target --exclude .git --exclude .claude --exclude .DS_Store /src/ ./
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --quiet
    status=0
    cargo test --workspace > /tmp/test.log 2>&1 || status=$?
    grep -E "^test result: .* [1-9][0-9]* passed|FAILED|panicked" /tmp/test.log || true
    [[ $status == 0 ]] || { tail -60 /tmp/test.log; exit $status; }
    cargo build --release --locked -p reprise --quiet'

echo "== systemd"
docker rm -f "$container" > /dev/null 2>&1 || true
trap 'docker rm -f "$container" > /dev/null 2>&1 || true' EXIT
docker run -d --name "$container" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  --tmpfs /run --tmpfs /run/lock -v "$volume":/opt/target:ro reprise-linux-systemd > /dev/null
for _ in $(seq 1 30); do
  state="$(docker exec "$container" systemctl is-system-running 2>/dev/null || true)"
  [[ "$state" == running || "$state" == degraded ]] && break
  sleep 1
done
docker exec "$container" bash -euo pipefail -c '
  install -m 755 /opt/target/release/reprise /usr/local/bin/reprise
  loginctl enable-linger dev
  systemctl start "user@$(id -u dev).service"'
docker cp "$here/scenario.sh" "$container":/usr/local/bin/scenario
answer="$(sed -n 's/^const ANSWER: &str = r#"\(.*\)"#;$/\1/p' apps/reprise/tests/sweep.rs)"
[[ -n "$answer" ]] || { echo "no stand-in answer in apps/reprise/tests/sweep.rs" >&2; exit 1; }
uid="$(docker exec "$container" id -u dev)"
docker exec -u dev -e HOME=/home/dev -e XDG_RUNTIME_DIR="/run/user/$uid" \
  -e DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" -w /home/dev "$container" \
  bash /usr/local/bin/scenario "$answer"
