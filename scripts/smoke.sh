#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture_dir=$(mktemp -d)
server_log=$(mktemp)
server_pid=""
port=${AUTOINDEX_SMOKE_PORT:-16701}

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf -- "$fixture_dir"
  rm -f -- "$server_log"
}
trap cleanup EXIT

mkdir "$fixture_dir/docs"
printf '%s\n' '# Smoke test' '' '> [!NOTE]' '> README rendering works.' > "$fixture_dir/README.md"
printf '%s' '0123456789' > "$fixture_dir/ten.txt"

cargo build --manifest-path "$project_dir/Cargo.toml" --locked
"$project_dir/target/debug/autoindex-rs" "$fixture_dir" --bind 127.0.0.1 --port "$port" >"$server_log" 2>&1 &
server_pid=$!

for _ in $(seq 1 50); do
  if curl --fail --silent "http://127.0.0.1:$port/" > /dev/null; then
    break
  fi
  sleep 0.1
done

page=$(curl --fail --silent "http://127.0.0.1:$port/")
range=$(curl --fail --silent --header 'Range: bytes=2-5' "http://127.0.0.1:$port/ten.txt")

grep -q 'Index of /' <<<"$page"
grep -q 'markdown-alert-note' <<<"$page"
test "$range" = '2345'
printf 'autoindex-rs smoke test passed on http://127.0.0.1:%s/\n' "$port"
