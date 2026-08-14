#!/usr/bin/env bash
# self-test of the journal reader.
#
# runs in the FAST CI: installs nothing, needs no root. it exists because the
# defect it covers does not show as an error but as SILENCE — a pattern that does
# not match returns zero results, and zero packages to verify looks like a
# passing check.
#
# the sample below is NOT invented: it reproduces the real log format, ANSI codes
# included (they are there even when the output is a pipe). both times this
# parsing broke, the cause was the same — a fixture written by looking at the CI
# web view, which renders the escapes invisible.
set -euo pipefail

cd "$(dirname "$0")"
# shellcheck source=scripts/ci/journal.sh
. ./journal.sh

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

DEP_STEP='install-system-dependencies'
FAILED=0

check() {
  local expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    printf '  ✔  %s\n' "$1"
  else
    printf '  ✖  %s\n     expected: %s\n     actual:   %s\n' "$1" "$expected" "$actual"
    FAILED=$((FAILED + 1))
  fi
}

# the real format: dimmed timestamp, level and target, italic field names, one
# field quoted and one NOT, running to end of line.
{
  printf '\033[2m2026-08-02T17:19:36Z\033[0m \033[32m INFO\033[0m \033[2minvok::progress\033[0m\033[2m:\033[0m ✔ prepare-opt-root\n'
  printf '\033[2m2026-08-02T17:19:37Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m package delta: packages added by us \033[3mstep\033[0m\033[2m=\033[0m"bootstrap-prerequisites" \033[3mpackages\033[0m\033[2m=\033[0mgit curl\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m package delta: packages added by us \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpackages\033[0m\033[2m=\033[0mbuild-essential libzip-dev node-less\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m package delta: packages already there, never touched \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpackages\033[0m\033[2m=\033[0mlibpq-dev zlib1g-dev\n'
  printf '\033[2m2026-08-02T17:19:39Z\033[0m \033[32m INFO\033[0m \033[2minvok::progress\033[0m\033[2m:\033[0m ✔ install-system-dependencies\n'
  # the Python plan. the fields come AFTER the message — the detail the first
  # pattern for this line got wrong.
  printf '\033[2m2026-08-02T17:19:40Z\033[0m \033[32m INFO\033[0m \033[2minvok::checks\033[0m\033[2m:\033[0m the system Python is newer than Odoo'\''s pins: the virtualenv will be built on a supported interpreter, installed for the purpose and removed by the rollback \033[3msystem_python\033[0m\033[2m=\033[0m3.14 \033[3minterpreter\033[0m\033[2m=\033[0mpython3.13 \033[3mpackages\033[0m\033[2m=\033[0mpython3.13 python3.13-devel\n'
} > "$WORK/raw.out"

echo "Self-test: reading the journal from the installer's output"

journal_strip_ansi "$WORK/raw.out" > "$WORK/clean.txt"
check "the ANSI escapes were stripped" \
  "0" "$(grep -c $'\033' "$WORK/clean.txt" || true)"

check "the completed steps are recognised" \
  "install-system-dependencies prepare-opt-root" \
  "$(journal_steps "$WORK/clean.txt" | tr '\n' ' ' | sed 's/ $//')"

check "the delta reads unquoted, to end of line" \
  "build-essential libzip-dev node-less" \
  "$(journal_packages "$WORK/clean.txt" 'packages added by us' "$DEP_STEP" | tr '\n' ' ' | sed 's/ $//')"

check "the chosen interpreter reads from the plan line" \
  "python3.13" "$(journal_python_plan "$WORK/clean.txt")"

check "with no plan line no interpreter is invented" \
  "" "$(journal_python_plan /dev/null)"

check "the pre-existing ones read from their own line" \
  "libpq-dev zlib1g-dev" \
  "$(journal_packages "$WORK/clean.txt" 'packages already there, never touched' "$DEP_STEP" | tr '\n' ' ' | sed 's/ $//')"

# the bootstrap delta stays installed on purpose: were it part of the
# dependencies' delta, the purge check would fail on minimal images, where those
# utilities are NOT preinstalled.
check "the bootstrap delta does not mix with the dependencies' one" \
  "" \
  "$(journal_packages "$WORK/clean.txt" 'packages added by us' "$DEP_STEP" | grep -x -e git -e curl || true)"

# without stripping ANSI, NOTHING is read: the defect that burned two CI rounds,
# so it is checked as such.
check "without stripping ANSI the parsing is blind (the original defect)" \
  "" "$(journal_steps "$WORK/raw.out" | tr '\n' ' ' | sed 's/ $//')"

# an empty result is legitimate and must NOT bring down the caller: pipefail is
# on in the integration script, and in probe mode the step may not have been
# reached.
: > "$WORK/empty.txt"
if ( set -euo pipefail; journal_packages "$WORK/empty.txt" 'packages added by us' "$DEP_STEP" >/dev/null ); then
  printf '  ✔  %s\n' "an empty result does not abort the caller (pipefail)"
else
  printf '  ✖  %s\n' "an empty result exits non-zero: under pipefail it would abort the CI"
  FAILED=$((FAILED + 1))
fi

if [ "$FAILED" -ne 0 ]; then
  echo "=== JOURNAL SELF-TEST FAILED: $FAILED checks did not pass ==="
  exit 1
fi
echo "Journal self-test: all green."
