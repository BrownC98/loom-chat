#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

CLIPPY_SIZE=3G
TEST_SIZE=4608M
DESKTOP_SIZE=3584M
WINDOWS_TARGET_DIR=${CARGO_TARGET_DIR:-target}/windows-gate

timing() {
  local name=$1 start=$2 status=$3 end
  end=$(date +%s%3N)
  printf 'CI_TIMING phase=%s start_ms=%s elapsed_ms=%s status=%s\n' "$name" "$start" "$((end - start))" "$status"
}

step() {
  local name=$1 start status=0
  shift
  echo "== $*"
  start=$(date +%s%3N)
  "$@" || status=$?
  timing "$name" "$start" "$status"
  return "$status"
}

unchanged_types() {
  local changed
  changed=$(git status --porcelain -- web/src/gen) || return
  [ -z "$changed" ] || { printf '%s\n' "$changed"; return 1; }
  diff -qr web/src/gen "$generated"
}

linux() {
  step linux-clippy memcap -m "$CLIPPY_SIZE" cargo clippy --all-targets
  step generated-types-copy cp -RL web/src/gen "$generated"
  TS_RS_EXPORT_DIR=$generated step linux-tests memcap -m "$TEST_SIZE" cargo test --lib --bin loom --test comments --test layers --test contract
  step generated-types unchanged_types
  step questions memcap -m "$TEST_SIZE" cargo test --test comments -- --ignored --exact asked_questions_were_answered
}

desktop() {
  cd desktop
  step desktop-fmt cargo fmt --check
  step desktop-clippy memcap -m "$DESKTOP_SIZE" cargo clippy --all-targets
  step desktop-test memcap -m "$DESKTOP_SIZE" cargo test
}

windows() {
  export CARGO_TARGET_DIR=$WINDOWS_TARGET_DIR
  step windows-gnu-clippy memcap -m "$CLIPPY_SIZE" cargo clippy --all-targets --target x86_64-pc-windows-gnu
}

web() {
  step web-check npm --prefix web run check
}

db_files() {
  local merged_changes
  merged_changes=$(git diff --name-status --diff-filter=MDR origin/main...HEAD -- 'src/db/changes/*.sql') || return
  [ -z "$merged_changes" ] || { echo "$merged_changes"; echo "add a new file in src/db/changes instead"; return 1; }
}

started=$(date +%s%3N)
logs=$(mktemp -d)
generated=$logs/generated
trap 'status=$?; timing total "$started" "$status"; rm -rf "$logs"' EXIT

step setup timeout 600 bash scripts/worktree/setup.sh
rm -f target/.rustc_info.json target/*/.rustc_info.json desktop/target/.rustc_info.json
step fmt cargo fmt --check
step db-files db_files
groups=(linux windows)
desktop_changes=$(git diff --name-only origin/main...HEAD -- desktop api fixtures web/src web/package-lock.json rust-toolchain.toml .cargo/config.toml Cargo.toml)
if [ -n "$desktop_changes" ]; then
  groups+=(desktop)
fi
groups+=(web)

pids=()
for group in "${groups[@]}"; do
  ("$group") >"$logs/$group" 2>&1 &
  pids+=("$!")
done
failed=
for index in "${!groups[@]}"; do
  group=${groups[index]}
  wait "${pids[index]}" || failed="$failed $group"
  echo "== $group group output"
  cat "$logs/$group"
done
[ -z "$failed" ] || { echo "== check group failed:$failed"; exit 1; }
echo "== all checks passed"
