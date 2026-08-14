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
  local atteso="$2" ottenuto="$3"
  if [ "$atteso" = "$ottenuto" ]; then
    printf '  ✔  %s\n' "$1"
  else
    printf '  ✖  %s\n     atteso:   %s\n     ottenuto: %s\n' "$1" "$atteso" "$ottenuto"
    FAILED=$((FAILED + 1))
  fi
}

# the real format: dimmed timestamp, level and target, italic field names, one
# field quoted and one NOT, running to end of line.
{
  printf '\033[2m2026-08-02T17:19:36Z\033[0m \033[32m INFO\033[0m \033[2minvok::progress\033[0m\033[2m:\033[0m ✔ prepare-opt-root\n'
  printf '\033[2m2026-08-02T17:19:37Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m delta pacchetti: pacchetti aggiunti da noi \033[3mstep\033[0m\033[2m=\033[0m"bootstrap-prerequisites" \033[3mpacchetti\033[0m\033[2m=\033[0mgit curl\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m delta pacchetti: pacchetti aggiunti da noi \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpacchetti\033[0m\033[2m=\033[0mbuild-essential libzip-dev node-less\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2minvok::steps::apt_packages\033[0m\033[2m:\033[0m delta pacchetti: pacchetti già presenti, mai toccati \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpacchetti\033[0m\033[2m=\033[0mlibpq-dev zlib1g-dev\n'
  printf '\033[2m2026-08-02T17:19:39Z\033[0m \033[32m INFO\033[0m \033[2minvok::progress\033[0m\033[2m:\033[0m ✔ install-system-dependencies\n'
  # the Python plan. the fields come AFTER the message — the detail the first
  # pattern for this line got wrong.
  printf '\033[2m2026-08-02T17:19:40Z\033[0m \033[32m INFO\033[0m \033[2minvok::checks\033[0m\033[2m:\033[0m il Python di sistema è più recente dei pin di Odoo: il virtualenv nascerà su un interprete supportato, installato per l\047occasione e rimosso dal rollback \033[3mpython_sistema\033[0m\033[2m=\033[0m3.14 \033[3minterprete\033[0m\033[2m=\033[0mpython3.13 \033[3mpacchetti\033[0m\033[2m=\033[0mpython3.13 python3.13-devel\n'
} > "$WORK/raw.out"

echo "Self-test: lettura del diario dall'output dell'installer"

journal_strip_ansi "$WORK/raw.out" > "$WORK/clean.txt"
check "gli escape ANSI sono stati rimossi" \
  "0" "$(grep -c $'\033' "$WORK/clean.txt" || true)"

check "gli step completati vengono riconosciuti" \
  "install-system-dependencies prepare-opt-root" \
  "$(journal_steps "$WORK/clean.txt" | tr '\n' ' ' | sed 's/ $//')"

check "il delta si legge senza virgolette, fino a fine riga" \
  "build-essential libzip-dev node-less" \
  "$(journal_packages "$WORK/clean.txt" 'pacchetti aggiunti da noi' "$DEP_STEP" | tr '\n' ' ' | sed 's/ $//')"

check "l'interprete scelto si legge dalla riga del piano" \
  "python3.13" "$(journal_python_plan "$WORK/clean.txt")"

check "senza riga del piano non si inventa un interprete" \
  "" "$(journal_python_plan /dev/null)"

check "i preesistenti si leggono dalla loro riga" \
  "libpq-dev zlib1g-dev" \
  "$(journal_packages "$WORK/clean.txt" 'pacchetti già presenti, mai toccati' "$DEP_STEP" | tr '\n' ' ' | sed 's/ $//')"

# the bootstrap delta stays installed on purpose: were it part of the
# dependencies' delta, the purge check would fail on minimal images, where those
# utilities are NOT preinstalled.
check "il delta di bootstrap non si mescola a quello delle dipendenze" \
  "" \
  "$(journal_packages "$WORK/clean.txt" 'pacchetti aggiunti da noi' "$DEP_STEP" | grep -x -e git -e curl || true)"

# without stripping ANSI, NOTHING is read: the defect that burned two CI rounds,
# so it is checked as such.
check "senza strip degli ANSI il parsing è cieco (il difetto originale)" \
  "" "$(journal_steps "$WORK/raw.out" | tr '\n' ' ' | sed 's/ $//')"

# an empty result is legitimate and must NOT bring down the caller: pipefail is
# on in the integration script, and in probe mode the step may not have been
# reached.
: > "$WORK/vuoto.txt"
if ( set -euo pipefail; journal_packages "$WORK/vuoto.txt" 'pacchetti aggiunti da noi' "$DEP_STEP" >/dev/null ); then
  printf '  ✔  %s\n' "un risultato vuoto non fa abortire il chiamante (pipefail)"
else
  printf '  ✖  %s\n' "un risultato vuoto fa uscire non-zero: sotto pipefail abortirebbe la CI"
  FAILED=$((FAILED + 1))
fi

if [ "$FAILED" -ne 0 ]; then
  echo "=== SELF-TEST DEL DIARIO FALLITO: $FAILED verifiche non superate ==="
  exit 1
fi
echo "Self-test del diario: tutto verde."
