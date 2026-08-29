#!/usr/bin/env bash
# Disposable loopback sshd and tmux. Never use the contributor's default socket.
set -euo pipefail
work=$(mktemp -d /tmp/starcom-ssh.XXXXXX)
chmod 700 "$work"
sshd_pid=""
agent_started=0
cleanup() {
    tmux -S "$work/tmux.sock" kill-server 2>/dev/null || true
    if [[ -f "$work/sshd.pid" ]]; then sudo kill "$(cat "$work/sshd.pid")" 2>/dev/null || true; fi
    if [[ -n "$sshd_pid" ]]; then sudo kill "$sshd_pid" 2>/dev/null || true; fi
    if [[ "$agent_started" == 1 ]]; then ssh-agent -k >/dev/null 2>&1 || true; fi
    rm -rf "$work"
}
trap cleanup EXIT
ssh-keygen -q -t ed25519 -N '' -f "$work/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$work/id_ed25519"
python3 - "$work/port" <<'PY'
import socket, sys
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    with open(sys.argv[1], 'w') as out:
        out.write(str(sock.getsockname()[1]))
PY
port=$(cat "$work/port")
user=$(id -un)
cat > "$work/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $work/host_key
PidFile $work/sshd.pid
AuthorizedKeysFile $work/id_ed25519.pub
AllowUsers $user
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
UsePAM yes
# Test fixture only: the unique authorized-key path lives under /tmp.
StrictModes no
PrintMotd no
LogLevel ERROR
EOF
sudo mkdir -p /run/sshd
sudo /usr/sbin/sshd -D -e -f "$work/sshd_config" > "$work/sshd.log" 2>&1 &
sshd_pid=$!
python3 - "$port" "$work/sshd.log" <<'PY'
import socket, sys, time
end = time.monotonic() + 10
while time.monotonic() < end:
    try:
        with socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=0.2):
            break
    except OSError:
        time.sleep(0.05)
else:
    raise SystemExit(open(sys.argv[2]).read() or 'fixture sshd did not start')
PY
# Trust is constructed from our generated server key, not from an unauthenticated
# ssh-keyscan result. All private fixture keys are deleted by the EXIT trap.
read -r kind key _ < "$work/host_key.pub"
printf '[127.0.0.1]:%s %s %s\n' "$port" "$kind" "$key" > "$work/known_hosts"
printf '@revoked [127.0.0.1]:%s %s %s\n' "$port" "$kind" "$key" > "$work/known_hosts.revoked"
cat "$work/known_hosts" >> "$work/known_hosts.revoked"
read -r kind key _ < "$work/id_ed25519.pub"
printf '[127.0.0.1]:%s %s %s\n' "$port" "$kind" "$key" > "$work/known_hosts.bad"
: > "$work/known_hosts.empty"
cp "$work/known_hosts" "$work/known_hosts.hashed"
ssh-keygen -H -f "$work/known_hosts.hashed" >/dev/null 2>&1
eval "$(ssh-agent -s)" >/dev/null
agent_started=1
ssh-add "$work/id_ed25519" >/dev/null 2>&1

env -u TMUX tmux -S "$work/tmux.sock" -f /dev/null new-session -d -s starcom -x 100 -y 30 \
    "printf '\033[2J\033[HSTARCOM_PRIMARY_READY\r\n'; exec sleep 600"
tmux -S "$work/tmux.sock" split-window -h -t starcom \
    "printf '\033[?1049h\033[2J\033[HSTARCOM_ALTERNATE_READY'; exec sleep 600"
tmux -S "$work/tmux.sock" set-option -t starcom update-environment STARCOM_TEST_ENV
tmux -S "$work/tmux.sock" set-environment -t starcom STARCOM_TEST_ENV original
# Bounded readiness check; do not race the PTYs' first output against capture.
for _ in $(seq 1 100); do
    count=$(tmux -S "$work/tmux.sock" list-panes -t starcom -F '#{pane_id}' | while read -r pane; do
        tmux -S "$work/tmux.sock" capture-pane -p -t "$pane"
    done | grep -c STARCOM_ || true)
    [[ "$count" == 2 ]] && break
    sleep 0.05
done
[[ "$count" == 2 ]] || { echo 'tmux fixture did not become ready' >&2; exit 1; }
export STARCOM_TEST_DIR="$work" STARCOM_TEST_USER="$user"
timeout 120s cargo test --test ssh_localhost -- --ignored --test-threads=1
