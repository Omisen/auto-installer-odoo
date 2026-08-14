#!/usr/bin/env bash
# =============================================================================
# the REAL integration test.
#
# really installs Odoo, checks it works, then runs the rollback and checks the
# system came back clean.
#
# it automates the manual VM session that found A-RT-1 (an install command that
# does not resolve dependencies, making installation impossible on any minimal
# system) and A-RT-2 (the rollback's purge failing on a broken dpkg, leaving the
# whole delta behind). neither was visible to the mock suite: mocks model what we
# know about the system, and those two bugs lived in what we did not.
#
# DESTRUCTIVE. it creates users, installs packages, touches PostgreSQL and
# systemd. run it ONLY on throwaway machines — CI runners, containers, test VMs.
# never on a working machine.
#
# modes:
#   full   the installation MUST succeed; the service, Odoo's HTTP answer and
#          the database are checked, then the rollback. needs a working systemd.
#   probe  the installation MAY fail (in a container systemd is not PID 1, so
#          the PostgreSQL step cannot start the service). what completed is
#          checked — the package names for that OS, the checksum pin for that
#          codename — and above all that the system stays clean. the portability
#          probe (A5.1, A5.2).
#
# variables: MODE, BIN, ENV_FILE. the expected artifact values follow the CI
# config.
# =============================================================================

set -euo pipefail

MODE="${MODE:-full}"
BIN="${BIN:-./target/release/invok}"
ENV_FILE="${ENV_FILE:-configs/ci.env}"

# the test adapts to the config it is given instead of assuming it: with nginx
# enabled it checks those steps too, which otherwise exit at the first condition
# and stay covered by mocks alone (B-V3-7).
#
# the file is read with `sed`, not sourced: the same reason the installer parses
# it declaratively — a `.env` is not code to execute.
env_value() {
  sed -n "s/^$1=[\"']\\?\\([^\"']*\\)[\"']\\?[[:space:]]*$/\\1/p" "$ENV_FILE" | tail -n 1
}
WITH_NGINX="$(env_value WITH_NGINX)"
WITH_NGINX="${WITH_NGINX:-false}"

# must match the CI config. the non-default database name is deliberate; see the
# comment in that file.
DB_NAME="${DB_NAME:-citest}"
DB_ROLE="${DB_ROLE:-odoo}"
OS_USER="${OS_USER:-odoo}"
PORT="${PORT:-8069}"
VER_SHORT="${VER_SHORT:-18}"

ODOO_HOME=/opt/odoo
INSTALL_DIR="$ODOO_HOME/odoo${VER_SHORT}"
UNIT="odoo${VER_SHORT}"
UNIT_FILE="/etc/systemd/system/${UNIT}.service"
STATE="/var/lib/invok/state.json"
# shellcheck source=scripts/ci/journal.sh
. "$(dirname "$0")/journal.sh"

WORK="$(mktemp -d)"

# assertions do NOT stop at the first failure: one CI round, which takes tens of
# minutes, must say *everything* that is wrong and not only the first symptom.
FAILURES=0
# COUNTING them is not enough. assertions live inside collapsed groups, so a
# reader sees "4 checks failed" and has to hunt for WHICH, opening groups one by
# one. A-R9-1's lesson applied to this script: a non-zero exit does not say why,
# and neither does a number. the messages accumulate and are reprinted at the
# end, OUTSIDE the groups.
FAILED_CHECKS=()

# --- helpers -----------------------------------------------------------------

group()  { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }
info()   { echo "  ·  $*"; }
ok()     { echo "  ✔  $*"; }
fail()   { echo "  ✖  $*"; FAILURES=$((FAILURES + 1)); FAILED_CHECKS+=("$*"); }

# reads the state file, which is root-owned and 0600.
state_json() { sudo cat "$STATE" 2>/dev/null || echo '{}'; }

# `assert <description> <command...>` — true when the command exits zero.
assert() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$desc"; else fail "$desc"; fi
}

# `refute <description> <command...>` — true when the command exits non-zero.
refute() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then fail "$desc"; else ok "$desc"; fi
}

pg_query() { sudo -u postgres psql -tAc "$1" 2>/dev/null || true; }

pg_reachable() { sudo -u postgres psql -tAc 'select 1' >/dev/null 2>&1; }

# the package manager's family, read from the system.
#
# the script runs on both: it makes per-family the three questions that depend on
# the manager — is it installed, what is installed, where is nginx's default site
# — and leaves the rest untouched, because the rest does not depend on it.
case "$(. /etc/os-release && echo "$ID")" in
  fedora|rhel|centos|almalinux|rocky) PKG_FAMILY=rpm ;;
  *)                                  PKG_FAMILY=deb ;;
esac
info "famiglia di pacchetti: $PKG_FAMILY"

# "installed" with the same definition the installer uses. not pedantry: the
# naive query exits **zero** for a removed package that still has its config
# files, and with that definition a failed purge could pass for a success. the
# assertions must measure what the installer considers present.
#
# on the other family the question does not arise — there is no "removed but
# configured" state — and the plain query is already exact.
pkg_installed() {
  if [ "$PKG_FAMILY" = rpm ]; then
    rpm -q -- "$1" >/dev/null 2>&1
  else
    dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'
  fi
}

# --- phase 1: installation ---------------------------------------------------

group "Installazione reale ($MODE)"
echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "Config: $ENV_FILE"

# a snapshot BEFORE mutating: does the perimeter directory already exist here?
# needed by the final check (A-V3-2). if it was pre-existing the rollback must
# leave it, and demanding it disappear would be demanding a violation. on runners
# and containers it should not exist, and then it must not exist at the end
# either.
if [ -d "$ODOO_HOME" ]; then
  OPT_ODOO_PREEXISTING=1
  info "$ODOO_HOME esisteva già prima dell'installazione"
else
  OPT_ODOO_PREEXISTING=0
  info "$ODOO_HOME assente prima dell'installazione (macchina vergine)"
fi

# was the system user already there? then the rollback must LEAVE it: the
# project's central protection applied to users, and it must be checked the right
# way round.
if id "$OS_USER" >/dev/null 2>&1; then
  OS_USER_PREEXISTING=1
  info "l'utente '$OS_USER' esisteva già prima dell'installazione"
else
  OS_USER_PREEXISTING=0
  info "l'utente '$OS_USER' assente prima dell'installazione"
fi

# what was in the default site's place before us (A-V3-5).
#
# on one family it is **not a separate file** but a block inside the main
# configuration, which the installer does not touch. there the question does not
# arise, and pretending it does would assert on a file that does not exist —
# green for the wrong reason.
if [ "$PKG_FAMILY" = rpm ]; then
  HAS_DEFAULT_SITE=0
  DEFAULT_SITE=""
  # there the drop-in directory is **already** the active one — nginx includes
  # it whole — so there is no symlink to enable.
  VHOST="/etc/nginx/conf.d/odoo${VER_SHORT}.conf"
  VHOST_LINK=""
else
  HAS_DEFAULT_SITE=1
  DEFAULT_SITE=/etc/nginx/sites-enabled/default
  VHOST="/etc/nginx/sites-available/odoo${VER_SHORT}"
  VHOST_LINK="/etc/nginx/sites-enabled/odoo${VER_SHORT}"
fi
if [ "$HAS_DEFAULT_SITE" = "0" ]; then
  DEFAULT_SITE_BEFORE="n/d (su rpm il default site non è un file separato)"
elif [ -L "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="symlink:$(readlink "$DEFAULT_SITE")"
elif [ -f "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="file"
else
  DEFAULT_SITE_BEFORE="assente"
fi
[ "$WITH_NGINX" = "true" ] && info "default site nginx prima dell'installazione: $DEFAULT_SITE_BEFORE"

# a snapshot of the installed packages BEFORE mutating.
#
# it feeds the most important final check, the one without bookkeeping: **no
# package that was there before may be missing after**. it does not depend on
# what we recorded, so it cannot pass for the wrong reason.
pkgs_installed_now() {
  if [ "$PKG_FAMILY" = rpm ]; then
    rpm -qa --qf '%{NAME}\n' 2>/dev/null | sort -u
  else
    dpkg-query -W -f='${Package}\t${Status}\n' 2>/dev/null \
      | awk -F'\t' '$2=="install ok installed"{print $1}' | sort -u
  fi
}
pkgs_installed_now > "$WORK/pkgs-before.txt"
info "pacchetti installati prima:     $(wc -l < "$WORK/pkgs-before.txt")"

# is the firewall ACTIVE? only then does the firewall step do anything: on a
# runner it is installed but inactive, and the step exits at once (so A-V3-7 was
# never exercised).
#
# the open ports, **one per line**, asked the way production asks. on one family
# the PERMANENT set is read and not the runtime one, because that is what the
# installer queries: asking the runtime could answer "open" where the installer
# sees "closed" and would measure something other than what it must protect.
fw_open_ports() {
  if [ "$PKG_FAMILY" = rpm ]; then
    sudo firewall-cmd --permanent --list-ports 2>/dev/null | tr ' ' '\n' | sed '/^$/d'
  else
    # the first column is the destination; the headers never match a token like
    # `80/tcp`, so there is no need to exclude them by name.
    sudo ufw status 2>/dev/null | awk '{print $1}'
  fi
}

if [ "$PKG_FAMILY" = rpm ]; then
  FW_NAME=firewalld
  if command -v firewall-cmd >/dev/null 2>&1 && sudo firewall-cmd --state >/dev/null 2>&1; then
    FW_ACTIVE=1
    sudo firewall-cmd --permanent --list-ports | sed 's/^/  firewalld· porte: /'
  else
    FW_ACTIVE=0
  fi
elif command -v ufw >/dev/null 2>&1 && sudo ufw status 2>/dev/null | grep -q "Status: active"; then
  FW_NAME=ufw
  FW_ACTIVE=1
  sudo ufw status | sed 's/^/  ufw· /'
else
  FW_NAME=ufw
  FW_ACTIVE=0
fi
# the scenario DECLARES that the firewall must be active.
#
# without this, a firewall that fails to come up skips the checks and leaves the
# job **green**: the risk is not a check that cannot fail but one that may not be
# RUN with nothing to say so — A-R9-1's variant ("in the scenario I wrote it for,
# does it run?") applied to a whole block of assertions.
#
# the scenario asking for the firewall is also the only one exercising it, so an
# immediate stop rather than a failure queued with the others: this is not an
# assertion about the installer, it is the scenario's precondition.
FW_REQUIRED="${FW_REQUIRED:-0}"
if [ "$WITH_NGINX" = "true" ] && [ "$FW_ACTIVE" = "0" ]; then
  if [ "$FW_REQUIRED" = "1" ]; then
    echo "::error::$FW_NAME non è attivo, ma questo scenario lo richiede \
(FW_REQUIRED=1): le verifiche su A-V3-7 verrebbero saltate e il job passerebbe \
senza aver provato ciò per cui esiste"
    exit 1
  fi
  info "$FW_NAME non attivo: le verifiche sul firewall verranno saltate"
fi
# the output is captured: the run's **journal** is read from it — which steps
# were reached, which packages added. the manifest does NOT serve this: it says
# what remains, and after a rollback nothing does.
set +e
sudo "$BIN" --config "$ENV_FILE" 2>&1 | tee "$WORK/install.out"
INSTALL_RC=${PIPESTATUS[0]}
set -e
echo "exit code installazione: $INSTALL_RC"
endgroup

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -ne 0 ]; then
  fail "l'installazione doveva riuscire (exit $INSTALL_RC)"
  # with no installation there is nothing to check, but the rollback is run
  # anyway: it must clean what the failed run left.
fi

# when a build fails, the error must SAY WHY (A-MD-7).
#
# a non-zero exit does not say why (A-R9-1): here the fix's whole value is in the
# text — the difference between three hundred lines of compiler output and "this
# Odoo version has no pin for this Python".
#
# **the expectation is derived from the installer's verdict, not from a threshold
# written here.** the preflight logs the warning only when the interpreter is
# newer than the tested ones; if that warning is present, a build failure MUST
# carry the diagnosis too. duplicating the threshold in shell would create a
# second source of truth that diverges silently (A-MD-5).
#
# outside that case nothing is asserted: a build failing on a covered Python has
# another cause, and demanding this diagnosis there would demand a wrong one.
#
# read from the output **without ANSI**: the logger colours on a pipe too, and a
# pattern written against what one sees on screen may not match what is in the
# file (A-R8-1-ter).
journal_strip_ansi "$WORK/install.out" > "$WORK/install-plain.txt"
if grep -q "più recente di Python" "$WORK/install-plain.txt"; then
  info "il preflight ha segnalato un interprete più recente di quelli provati"
  if grep -q "Building wheel for gevent" "$WORK/install-plain.txt"; then
    assert "il fallimento di gevent spiega che è il Python" \
      grep -q "non regge gli header di un CPython più nuovo" "$WORK/install-plain.txt"
  fi
fi

# the alternative interpreter (M11), checked **where it left traces**.
#
# as above, the expectation comes from the installer's verdict and not from a
# version written here: if the preflight says it chose another interpreter, that
# name comes out of the log and everything follows — which binary must be in the
# virtualenv and which package must be gone after the rollback. writing a version
# in this script would be a second table ageing on its own (A-MD-5), and would
# fail the jobs where the system interpreter is perfectly fine.
PYTHON_PLAN="$(journal_python_plan "$WORK/install-plain.txt")"
if [ -n "$PYTHON_PLAN" ]; then
  info "l'installer ha scelto l'interprete '$PYTHON_PLAN' per il virtualenv"
  if [ "$INSTALL_RC" -eq 0 ]; then
    # the virtualenv carries the base interpreter's binary: proof it was born
    # FROM THAT one and not from the system's. privileged, because the perimeter
    # is 0750 and an unprivileged test would answer "permission denied" — a red
    # for the wrong reason.
    assert "il virtualenv è nato su $PYTHON_PLAN" \
      sudo test -x "$INSTALL_DIR/sandbox/bin/$PYTHON_PLAN"
    assert "l'interprete scelto è installato sul sistema" \
      pkg_installed "$PYTHON_PLAN"
  fi
fi

# --- phase 2: the installed system works (full mode only) --------------------

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
  group "Verifica dell'installazione"

  assert "utente di sistema '$OS_USER' creato" id "$OS_USER"
  # privileged, and not out of habit: the perimeter is 0750, so the user running
  # this script may not TRAVERSE it. an unprivileged test there does not answer
  # "absent", it answers "permission denied" — and the assertion turns both into
  # the same red.
  #
  # on native runners these passed, which means they passed by a property of the
  # environment and not because the question was well put; on another family the
  # bill arrived. a check must be made with the privileges the question needs, or
  # it measures the permissions of whoever runs it.
  assert "sorgenti in $INSTALL_DIR" sudo test -d "$INSTALL_DIR/odoo"
  assert "virtualenv creato" sudo test -x "$INSTALL_DIR/sandbox/bin/python3"
  assert "config generata" sudo test -f "$INSTALL_DIR/odoo${VER_SHORT}.conf"
  assert "unit systemd installata" test -f "$UNIT_FILE"
  assert "servizio attivo" systemctl is-active --quiet "$UNIT"
  assert "servizio abilitato" systemctl is-enabled --quiet "$UNIT"
  assert "wkhtmltopdf installato" pkg_installed wkhtmltox

  # the database exists and carries an initialised schema.
  if [ "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" = "1" ]; then
    ok "database '$DB_NAME' creato"
  else
    fail "database '$DB_NAME' assente"
  fi
  if [ "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" = "1" ]; then
    ok "ruolo PostgreSQL '$DB_ROLE' creato"
  else
    fail "ruolo PostgreSQL '$DB_ROLE' assente"
  fi
  if [ "$(sudo -u postgres psql -tAd "$DB_NAME" -c \
        "select 1 from information_schema.tables where table_name='ir_module_module'" 2>/dev/null)" = "1" ]; then
    ok "schema Odoo inizializzato (ir_module_module presente)"
  else
    fail "schema Odoo non inizializzato"
  fi

  # Odoo really answers on the port. an "active" service is not enough: the
  # process can be up and die right after on a bad config. the root path answers
  # with a redirect, which is proof enough of life.
  http=""
  for _ in $(seq 1 30); do
    http="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/" 2>/dev/null || echo 000)"
    case "$http" in 200|301|302|303|307) break ;; esac
    sleep 2
  done
  case "$http" in
    200|301|302|303|307) ok "Odoo risponde su :$PORT (HTTP $http)" ;;
    *) fail "Odoo non risponde su :$PORT (ultimo codice: ${http:-nessuno})"
       sudo journalctl -u "$UNIT" -n 60 --no-pager || true ;;
  esac

  # A-R5-1: the state survives success and is marked finished. that is what
  # makes a later uninstall possible; if this fails, the rollback below would
  # have nothing to consume.
  assert "il manifesto di disinstallazione è rimasto sul disco" sudo test -f "$STATE"
  if [ "$(state_json | jq -r '.finished')" = "true" ]; then
    ok "il manifesto è marcato 'finished'"
  else
    fail "il manifesto NON è marcato 'finished': il rollback lo leggerebbe come \
installazione interrotta"
  fi
  if [ "$(state_json | jq -r '.config.db_name')" = "$DB_NAME" ]; then
    ok "il manifesto porta la configurazione reale (db_name=$DB_NAME)"
  else
    fail "il manifesto non porta il db_name reale: il rollback non saprebbe cosa \
rimuovere (regressione A-R4-1)"
  fi

  endgroup
fi

# --- phase 2c: the nginx phase, when asked for (B-V3-7) ----------------------
#
# until this existed the real CI had **never** run it: the base config disables
# nginx, so those steps exited at the first condition in every installation ever
# done on a real machine. two whole remediations lived entirely on mocks — and
# that is exactly the situation in which defects have survived here.

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ] && [ "$WITH_NGINX" = "true" ]; then
  group "Verifica Nginx"

  assert "vhost generato in $VHOST" sudo test -f "$VHOST"
  if [ -n "$VHOST_LINK" ]; then
    assert "sito abilitato ($VHOST_LINK)" sudo test -L "$VHOST_LINK"
  else
    info "nessun symlink da verificare: su rpm il vhost vive già in conf.d"
  fi
  assert "la configurazione nginx è valida (nginx -t)" sudo nginx -t
  assert "nginx attivo" systemctl is-active --quiet nginx

  # A-V3-12: the vhost's logs carry the version, not a hardcoded one.
  if sudo grep -q "odoo${VER_SHORT}.access.log" "$VHOST"; then
    ok "i log del vhost seguono la versione installata (A-V3-12)"
  else
    fail "i log del vhost non portano la versione: due istanze si scriverebbero addosso"
    sudo grep -n "access_log\|error_log" "$VHOST" || true
  fi

  # A-V3-6: the vhost promises no TLS. a 443 block towards non-existent
  # certificates would fail validation — which above it passes.
  if sudo grep -qE "^\s*listen\s+443|^\s*ssl_certificate" "$VHOST"; then
    fail "il vhost contiene direttive TLS: non le genera l'installer (A-V3-6)"
  else
    ok "il vhost non promette TLS (è compito di certbot --nginx)"
  fi

  # the default site was moved out of the way, which is what frees port 80.
  if [ "$HAS_DEFAULT_SITE" = "1" ]; then
    refute "il default site è stato disattivato" sudo test -e "$DEFAULT_SITE"
  fi

  # the firewall: port 80 must have been opened (A-V3-7).
  #
  # this is where the token comparison is proved on a real machine. with the old
  # substring check, a similar-looking rule made port 80 look already open: it
  # never entered the delta, was never opened, and nginx stayed unreachable
  # **with no error at all**.
  if [ "$FW_ACTIVE" = "1" ]; then
    if fw_open_ports | grep -qx "80/tcp"; then
      ok "regola $FW_NAME 80/tcp aperta (A-V3-7: non confusa con 8080/tcp)"
    else
      fail "la porta 80 NON è stata aperta su $FW_NAME: nginx resta irraggiungibile"
      fw_open_ports | sed 's/^/    porta· /' || true
    fi
  fi

  # and port 80 serves Odoo through the proxy: the only proof the whole chain
  # really works.
  http80=""
  for _ in $(seq 1 15); do
    http80="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1/" 2>/dev/null || echo 000)"
    case "$http80" in 200|301|302|303|307) break ;; esac
    sleep 2
  done
  case "$http80" in
    200|301|302|303|307) ok "Odoo risponde attraverso Nginx sulla porta 80 (HTTP $http80)" ;;
    *) fail "la porta 80 non serve Odoo (ultimo codice: ${http80:-nessuno})"
       sudo nginx -T 2>/dev/null | head -n 40 || true ;;
  esac

  endgroup
fi

# --- phase 2b: a second installation must NOT touch the manifest -------------
#
# A-V3-1. re-running the installer over an existing instance used to rewrite the
# manifest with every artifact marked pre-existing — correctly, since that is
# what the snapshots see — after which the rollback did nothing, declared no
# leftovers, cleared the state and left Odoo installed forever. the scenario
# ended **green**, which is why it is proved here and not only on mocks.
#
# three things, in order of importance: that the second run fails; that the
# manifest is **identical**; and that the message sends the user somewhere
# instead of leaving them to guess.

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
  group "Seconda installazione: deve essere rifiutata (A-V3-1)"

  sudo cp "$STATE" "$WORK/manifest-before.json"

  set +e
  sudo "$BIN" --config "$ENV_FILE" > "$WORK/second-install.log" 2>&1
  SECOND_RC=$?
  set -e
  echo "exit code seconda installazione: $SECOND_RC"

  if [ "$SECOND_RC" -ne 0 ]; then
    ok "la seconda installazione è stata rifiutata (exit $SECOND_RC)"
  else
    fail "la seconda installazione è stata ACCETTATA: il manifesto è a rischio (A-V3-1)"
  fi

  sudo cp "$STATE" "$WORK/manifest-after.json"
  # the comparison exits non-zero both when the files DIFFER and when it cannot
  # compare them at all. telling the two apart is not pedantry: the tool was
  # missing from one image and this block accused the manifest of having changed
  # when nobody had looked at it. a check that cannot run must say "I could not",
  # never "it went badly" — the blindness-versus-absence distinction again.
  if ! command -v cmp >/dev/null 2>&1; then
    fail "impossibile confrontare il manifesto: 'cmp' non è installato \
(pacchetto diffutils). Il controllo su A-V3-1 NON è stato eseguito"
  elif sudo cmp -s "$WORK/manifest-before.json" "$WORK/manifest-after.json"; then
    ok "il manifesto è rimasto identico byte-per-byte"
  else
    fail "il manifesto è cambiato dopo un'installazione rifiutata: \
l'istanza potrebbe non essere più disinstallabile (A-V3-1)"
    sudo diff "$WORK/manifest-before.json" "$WORK/manifest-after.json" 2>/dev/null || true
  fi

  # the refusal must come from the MANIFEST, not from a side effect.
  #
  # A-R9-1: this block's first version settled for a non-zero exit and a message
  # naming the rollback command. it failed for the wrong reason — the port check
  # rejected the installation first, because Odoo was listening, and the manifest
  # check was never reached. "free the port" sends the user to stop Odoo, not to
  # uninstall it.
  if grep -q "installazione completata su questa macchina" "$WORK/second-install.log"; then
    ok "il rifiuto viene dal manifesto (A-V3-1), non da un effetto collaterale"
  else
    fail "la seconda installazione è stata respinta, ma NON dal controllo sul manifesto: \
il rifiuto di A-V3-1 non è stato raggiunto (A-R9-1)"
    tail -n 20 "$WORK/second-install.log" || true
  fi

  if grep -q -e 'porta .* in uso' "$WORK/second-install.log"; then
    fail "il rifiuto arriva dal controllo sulla porta: è una CONSEGUENZA \
dell'installazione esistente, non la causa — l'utente viene mandato a fermare Odoo (A-R9-1)"
  else
    ok "nessuna diagnosi fuorviante sulla porta"
  fi

  if grep -q -e 'rollback' -e '--force' "$WORK/second-install.log"; then
    ok "il rifiuto indica come procedere (rollback o --force)"
  else
    fail "il rifiuto non dice all'utente cosa fare"
    tail -n 20 "$WORK/second-install.log" || true
  fi

  endgroup
fi

# --- phase 3: the run's journal, read from the LOG ---------------------------
#
# the delta is the set of packages that were NOT there before us: the rollback
# must purge those and ONLY those. reading it from the output instead of writing
# it by hand makes the assertion independent of the image — a hosted runner has
# the build tools preinstalled, a minimal container does not.
#
# **from the log and not from the manifest**, and the distinction is the point.
# the manifest says *what is still on the system*: when a rollback undoes a step
# that record disappears, which is what stops a re-run from skipping artifacts
# that no longer exist (A-R8-1). the account of *what was done* lives in the log,
# which is not rewritten. in probe mode the installation fails and undoes itself,
# so the manifest is rightly empty here: reading it would give zero packages and
# every cleanliness check would pass on nothing.

group "Diario dell'esecuzione (dal log)"

# the parsing lives in journal.sh, exercised by its self-test in the fast CI: a
# pattern that does not match gives no error, it gives zero results — and zero
# packages to verify looks like a passing check.
sed_out="$WORK/install.txt"
journal_strip_ansi "$WORK/install.out" > "$sed_out"

readonly DEP_STEP='install-system-dependencies'
journal_steps "$sed_out" > "$WORK/steps.txt"
journal_packages "$sed_out" 'pacchetti aggiunti da noi' "$DEP_STEP" > "$WORK/delta.txt"
journal_packages "$sed_out" 'pacchetti già presenti, mai toccati' "$DEP_STEP" \
  > "$WORK/preexisting.txt"

info "step completati:                $(wc -l < "$WORK/steps.txt")"
info "delta (installati da noi):      $(wc -l < "$WORK/delta.txt") pacchetti"
info "preesistenti (mai toccati):     $(wc -l < "$WORK/preexisting.txt") pacchetti"

if [ ! -s "$WORK/delta.txt" ] && [ "$MODE" = "full" ]; then
  fail "nessun pacchetto nel delta: il diario non è stato letto correttamente, \
e le verifiche di pulizia passerebbero a vuoto"
fi

# in probe mode the installation stops early by construction. without this
# check, a failure at step one would pass every cleanliness check for the wrong
# reason: nothing to clean because nothing was done. these two steps are what the
# probe exists to exercise — that OS's package names (A5.1) and its codename's
# checksum pin (A5.2, A-RT-1).
if [ "$MODE" = "probe" ]; then
  for step in install-system-dependencies install-wkhtmltopdf; do
    if grep -qx "$step" "$WORK/steps.txt"; then
      ok "la sonda ha superato '$step' su questo OS"
    else
      fail "la sonda non ha raggiunto '$step': la portabilità non è stata verificata"
    fi
  done
fi
endgroup

# --- phase 4: rollback -------------------------------------------------------

group "Rollback"
set +e
sudo "$BIN" rollback --yes 2>&1 | tee "$WORK/rollback.out"
ROLLBACK_RC=${PIPESTATUS[0]}
set -e
echo "exit code rollback: $ROLLBACK_RC"
endgroup

if [ "$ROLLBACK_RC" -eq 0 ]; then
  ok "il rollback è terminato senza errori"
else
  fail "il rollback è uscito con $ROLLBACK_RC"
fi
# two outcomes are both correct, and telling them apart matters.
#
# "no leftovers" means there was a registered installation and all of it was
# undone. "nothing to undo" means there was nothing to do — the NORMAL case in
# probe mode, where the installation fails and **undoes itself** during the run,
# so by the time we get here the system is already clean and the manifest gone.
# demanding the first would demand that something was still left.
if grep -q "Nessun residuo" "$WORK/rollback.out"; then
  ok "il rollback dichiara nessun residuo"
elif grep -q "Nessuna installazione da annullare" "$WORK/rollback.out"; then
  if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
    fail "non c'era nulla da annullare, ma l'installazione era riuscita: il manifesto \
di disinstallazione è sparito quando invece doveva restare"
  else
    ok "niente da annullare: il rollback in-process aveva già ripulito tutto"
  fi
else
  fail "il rollback ha lasciato residui (vedi il report qui sopra)"
fi

# --- phase 5: the system is clean again --------------------------------------
#
# the same checks once done by hand on a VM, now automatic. the point is not that
# the command exited zero: it is that nothing of ours is left on the system.

group "Verifica di pulizia post-rollback"

if [ "$OS_USER_PREEXISTING" = "1" ]; then
  # pre-existing: NOT ours to delete. the anti-drop applied to users, and here
  # the assertion runs the other way round.
  assert "l'utente preesistente '$OS_USER' è sopravvissuto al rollback" id "$OS_USER"
else
  refute "l'utente di sistema '$OS_USER' è stato rimosso" id "$OS_USER"
  refute "il gruppo '$OS_USER' è stato rimosso" getent group "$OS_USER"
fi
refute "la directory di installazione è sparita" test -d "$INSTALL_DIR"
refute "l'unit systemd è stata rimossa" test -f "$UNIT_FILE"
refute "il servizio non è più attivo" systemctl is-active --quiet "$UNIT"
refute "il manifesto è stato consumato" sudo test -f "$STATE"
refute "wkhtmltopdf è stato purgato" pkg_installed wkhtmltox

# the alternative interpreter goes with everything else (M11).
#
# the half that makes the choice *reversible*: tens of megabytes installed by us
# and never removed would be a leftover inside the perimeter the rollback
# promises to restore — which is why the step that carries it is the one that
# purges its delta and not the one that leaves what it adds.
if [ -n "$PYTHON_PLAN" ]; then
  refute "l'interprete '$PYTHON_PLAN' installato da noi è stato rimosso" \
    pkg_installed "$PYTHON_PLAN"
fi

if pg_reachable; then
  if [ -z "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" ]; then
    ok "database '$DB_NAME' droppato"
  else
    fail "database '$DB_NAME' ancora presente"
  fi
  if [ -z "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" ]; then
    ok "ruolo PostgreSQL '$DB_ROLE' rimosso"
  else
    fail "ruolo PostgreSQL '$DB_ROLE' ancora presente"
  fi
else
  info "PostgreSQL non raggiungibile: verifiche su DB/ruolo saltate"
  info "(atteso in MODE=probe, dove il servizio non è mai partito)"
fi

# PostgreSQL stays INSTALLED: without the aggressive flag the rollback only
# stops and disables, because those are reversible and a purge is not (D3). that
# it stays is correct behaviour, not a leftover.
if pkg_installed postgresql; then
  ok "PostgreSQL resta installato (corretto: purge solo con --aggressive-rollback)"
fi

# the customer's nginx config comes back as it was (B-V3-7).
#
# A-V3-5's half that counts. the default site is **somebody else's pre-existing
# configuration**: removing it to free port 80 is lawful, not putting it back is
# not. until this phase no real run had ever checked it.
if [ "$WITH_NGINX" = "true" ]; then
  refute "il vhost è stato rimosso" sudo test -f "$VHOST"
  if [ -n "$VHOST_LINK" ]; then
    refute "il sito è stato disabilitato" sudo test -e "$VHOST_LINK"
  fi

  # on that family the default site is not a separate file: there is no return
  # to verify, and inventing the assertion would give a green for the wrong
  # reason.
  if [ "$HAS_DEFAULT_SITE" = "1" ]; then
  case "$DEFAULT_SITE_BEFORE" in
    assente)
      refute "nessun default site inventato dal rollback" sudo test -e "$DEFAULT_SITE"
      ;;
    symlink:*)
      atteso="${DEFAULT_SITE_BEFORE#symlink:}"
      if [ -L "$DEFAULT_SITE" ] && [ "$(readlink "$DEFAULT_SITE")" = "$atteso" ]; then
        ok "il default site è tornato al suo target originale ($atteso)"
      else
        fail "il default site NON è stato ripristinato com'era: atteso symlink → $atteso, \
trovato $( [ -e "$DEFAULT_SITE" ] && ls -ld "$DEFAULT_SITE" || echo assente )"
      fi
      ;;
    file)
      if [ -f "$DEFAULT_SITE" ] && [ ! -L "$DEFAULT_SITE" ]; then
        ok "il default site (file regolare) è stato rimesso al suo posto"
      else
        fail "un default site che era un FILE non è tornato tale: è la perdita di \
configurazione che A-V3-5 descrive"
      fi
      ;;
  esac
  fi

  # after the rollback: close what we opened, and **only** that. a customer's
  # pre-existing rule is never touched — the package delta's rule, applied to the
  # firewall.
  if [ "$FW_ACTIVE" = "1" ]; then
    if fw_open_ports | grep -qx "80/tcp"; then
      fail "la regola 80/tcp aperta da noi è rimasta dopo il rollback ($FW_NAME)"
    else
      ok "la regola $FW_NAME 80/tcp è stata richiusa"
    fi
    if fw_open_ports | grep -qx "8080/tcp"; then
      ok "la regola preesistente 8080/tcp è intatta ($FW_NAME)"
    else
      fail "il rollback ha rimosso una regola $FW_NAME che non aveva aperto lui"
    fi
  fi

  # nginx survives and stays serviceable: the rollback must not leave the
  # customer's service with a broken config (A1.4).
  if pkg_installed nginx || pkg_installed nginx-core || pkg_installed nginx-full; then
    ok "nginx resta installato (purge solo con --aggressive-rollback)"
  fi
  assert "la configurazione nginx è valida anche dopo il rollback" sudo nginx -t
fi

# the package delta: purged in full.
delta_left=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if pkg_installed "$pkg"; then
    fail "pacchetto del delta ancora installato: $pkg"
    delta_left=$((delta_left + 1))
  fi
done < "$WORK/delta.txt"
# an `if`, not a short-circuit: under `set -e` a false test as the last command
# of an `&&` list would exit the script before the final report.
if [ "$delta_left" -eq 0 ]; then
  ok "tutti i pacchetti del delta sono stati purgati"
fi

# the pre-existing ones: never touched. the surgical promise from the other
# side — a rollback that purges too much is as bad as one that purges too little.
preexisting_lost=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if ! pkg_installed "$pkg"; then
    fail "pacchetto PREESISTENTE rimosso dal rollback: $pkg"
    preexisting_lost=$((preexisting_lost + 1))
  fi
done < "$WORK/preexisting.txt"
if [ "$preexisting_lost" -eq 0 ]; then
  ok "nessun pacchetto preesistente è stato toccato"
fi

# the same promise **without bookkeeping**: the set of installed packages before
# is compared with the set now. it does not depend on what we recorded, so it
# cannot pass for the wrong reason — the difference between "the packages we said
# we added are gone" and "nothing that was there is gone".
pkgs_installed_now > "$WORK/pkgs-after.txt"
if perduti="$(comm -23 "$WORK/pkgs-before.txt" "$WORK/pkgs-after.txt")" && [ -z "$perduti" ]; then
  ok "nessun pacchetto presente prima dell'installazione è stato rimosso"
else
  fail "il rollback ha rimosso pacchetti che c'erano già: $(echo "$perduti" | tr '\n' ' ')"
fi

# on a virgin machine the perimeter directory must NOT exist after the rollback.
#
# until A-V3-2 this check accepted a whitelist and called a surviving directory
# fine: it wrote the leftover down as expected behaviour, which is why the CI
# never found the defect. those files were there because the lock and the log
# lived inside the home; now they live outside it, the directory has no reason to
# survive, and the first step's undo can finally fire.
#
# this line is what found A-R5-3: the pip cache and the filestore, both born in
# the home outside any step and therefore not undoable. closed from opposite
# sides — the cache no longer appears there, the filestore is now a step with its
# own PreState. the regression guard stays, tighter: any content is a leftover,
# and so is the directory itself.
#
# it holds because the runner starts without that directory. were it
# pre-existing, NOT removing it would be the correct behaviour — hence the
# snapshot taken before installing.
if [ "${OPT_ODOO_PREEXISTING:-0}" = "1" ]; then
  ok "$ODOO_HOME era preesistente: il rollback non deve rimuoverla (nessuna asserzione)"
elif [ -d "$ODOO_HOME" ]; then
  leftovers="$(sudo ls -A "$ODOO_HOME" | tr '\n' ' ' || true)"
  fail "$ODOO_HOME è sopravvissuta al rollback (contenuto: ${leftovers:-vuota}) — A-V3-2"
else
  ok "$ODOO_HOME non esiste più: il perimetro è tornato com'era"
fi

endgroup

# --- outcome -----------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== INTEGRAZIONE OK ($MODE): installazione e rollback verificati sul sistema reale ==="
  exit 0
fi
echo "=== INTEGRAZIONE FALLITA ($MODE): $FAILURES verifiche non superate ==="
echo "Verifiche fallite:"
for check in "${FAILED_CHECKS[@]}"; do
  echo "  ✖  $check"
  # an error annotation shows at the top of the run: visible without opening
  # anything, which is the point.
  echo "::error::$check"
done
echo "Log dell'installer:"
sudo tail -n 100 /var/log/invok.log 2>/dev/null || echo "(nessun log)"
exit 1
