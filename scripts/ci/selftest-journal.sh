#!/usr/bin/env bash
# Self-test della lettura del diario (scripts/ci/journal.sh).
#
# Gira nella CI VELOCE: non installa niente, non serve root. Esiste perché il
# difetto che copre non si manifesta come errore ma come SILENZIO — un pattern
# che non combacia restituisce zero risultati, e zero pacchetti da verificare si
# presenta come una verifica superata.
#
# Il campione qui sotto NON è inventato: riproduce il formato reale di `tracing`,
# codici ANSI inclusi (che ci sono anche quando l'output è una pipe). Le due
# volte in cui questo parsing si è rotto, la causa è stata la stessa — un fixture
# scritto guardando i log di GitHub, che gli escape li rende invisibili.
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

# Formato reale: timestamp/livello/target dimmati, nomi dei campi in corsivo,
# `step` quotato (Debug) e `pacchetti` NON quotato (Display), fino a fine riga.
{
  printf '\033[2m2026-08-02T17:19:36Z\033[0m \033[32m INFO\033[0m \033[2modoo_installer::progress\033[0m\033[2m:\033[0m ✔ prepare-opt-root\n'
  printf '\033[2m2026-08-02T17:19:37Z\033[0m \033[32m INFO\033[0m \033[2modoo_installer::steps::apt_packages\033[0m\033[2m:\033[0m delta apt: pacchetti aggiunti da noi \033[3mstep\033[0m\033[2m=\033[0m"bootstrap-prerequisites" \033[3mpacchetti\033[0m\033[2m=\033[0mgit curl\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2modoo_installer::steps::apt_packages\033[0m\033[2m:\033[0m delta apt: pacchetti aggiunti da noi \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpacchetti\033[0m\033[2m=\033[0mbuild-essential libzip-dev node-less\n'
  printf '\033[2m2026-08-02T17:19:38Z\033[0m \033[32m INFO\033[0m \033[2modoo_installer::steps::apt_packages\033[0m\033[2m:\033[0m delta apt: pacchetti già presenti, mai toccati \033[3mstep\033[0m\033[2m=\033[0m"install-system-dependencies" \033[3mpacchetti\033[0m\033[2m=\033[0mlibpq-dev zlib1g-dev\n'
  printf '\033[2m2026-08-02T17:19:39Z\033[0m \033[32m INFO\033[0m \033[2modoo_installer::progress\033[0m\033[2m:\033[0m ✔ install-system-dependencies\n'
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

check "i preesistenti si leggono dalla loro riga" \
  "libpq-dev zlib1g-dev" \
  "$(journal_packages "$WORK/clean.txt" 'pacchetti già presenti, mai toccati' "$DEP_STEP" | tr '\n' ' ' | sed 's/ $//')"

# Il delta di bootstrap resta installato di proposito: se finisse nel delta di
# install-system-dependencies, la verifica di purga fallirebbe sulle immagini
# minimali, dove git/curl NON sono preinstallati.
check "il delta di bootstrap non si mescola a quello delle dipendenze" \
  "" \
  "$(journal_packages "$WORK/clean.txt" 'pacchetti aggiunti da noi' "$DEP_STEP" | grep -x -e git -e curl || true)"

# Senza togliere gli ANSI non si legge NIENTE: è il difetto che ha bruciato due
# giri di CI, e va verificato che sia proprio quello.
check "senza strip degli ANSI il parsing è cieco (il difetto originale)" \
  "" "$(journal_steps "$WORK/raw.out" | tr '\n' ' ' | sed 's/ $//')"

# Un risultato vuoto è legittimo e NON deve far cadere lo script chiamante:
# `set -o pipefail` è attivo in integration-test.sh, e in MODE=probe lo step può
# non essere stato raggiunto.
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
