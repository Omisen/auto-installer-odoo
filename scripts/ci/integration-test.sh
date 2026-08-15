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
info "package family: $PKG_FAMILY"

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

group "Real installation ($MODE)"
echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "Config: $ENV_FILE"

# a snapshot BEFORE mutating: does the perimeter directory already exist here?
# needed by the final check (A-V3-2). if it was pre-existing the rollback must
# leave it, and demanding it disappear would be demanding a violation. on runners
# and containers it should not exist, and then it must not exist at the end
# either.
if [ -d "$ODOO_HOME" ]; then
  OPT_ODOO_PREEXISTING=1
  info "$ODOO_HOME already existed before the installation"
else
  OPT_ODOO_PREEXISTING=0
  info "$ODOO_HOME absent before the installation (virgin machine)"
fi

# was the system user already there? then the rollback must LEAVE it: the
# project's central protection applied to users, and it must be checked the right
# way round.
if id "$OS_USER" >/dev/null 2>&1; then
  OS_USER_PREEXISTING=1
  info "user '$OS_USER' already existed before the installation"
else
  OS_USER_PREEXISTING=0
  info "user '$OS_USER' absent before the installation"
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
  DEFAULT_SITE_BEFORE="n/a (on this family the default site is not a separate file)"
elif [ -L "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="symlink:$(readlink "$DEFAULT_SITE")"
elif [ -f "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="file"
else
  DEFAULT_SITE_BEFORE="absent"
fi
[ "$WITH_NGINX" = "true" ] && info "nginx default site before the installation: $DEFAULT_SITE_BEFORE"

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
info "packages installed before:      $(wc -l < "$WORK/pkgs-before.txt")"

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
    echo "::error::$FW_NAME is not active, but this scenario requires it \
(FW_REQUIRED=1): the A-V3-7 checks would be skipped and the job would pass \
without having tested what it exists for"
    exit 1
  fi
  info "$FW_NAME not active: the firewall checks will be skipped"
fi
# the output is captured: the run's **journal** is read from it — which steps
# were reached, which packages added. the manifest does NOT serve this: it says
# what remains, and after a rollback nothing does.
set +e
sudo "$BIN" --config "$ENV_FILE" 2>&1 | tee "$WORK/install.out"
INSTALL_RC=${PIPESTATUS[0]}
set -e
echo "installation exit code: $INSTALL_RC"
endgroup

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -ne 0 ]; then
  fail "the installation should have succeeded (exit $INSTALL_RC)"
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
if grep -q "newer than Python" "$WORK/install-plain.txt"; then
  info "the preflight reported an interpreter newer than the tested ones"
  if grep -q "Building wheel for gevent" "$WORK/install-plain.txt"; then
    assert "the gevent failure explains that Python is the cause" \
      grep -q "does not survive a newer CPython" "$WORK/install-plain.txt"
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
  info "the installer chose interpreter '$PYTHON_PLAN' for the virtualenv"
  if [ "$INSTALL_RC" -eq 0 ]; then
    # the virtualenv carries the base interpreter's binary: proof it was born
    # FROM THAT one and not from the system's. privileged, because the perimeter
    # is 0750 and an unprivileged test would answer "permission denied" — a red
    # for the wrong reason.
    assert "the virtualenv was built on $PYTHON_PLAN" \
      sudo test -x "$INSTALL_DIR/sandbox/bin/$PYTHON_PLAN"
    assert "the chosen interpreter is installed on the system" \
      pkg_installed "$PYTHON_PLAN"
  fi
fi

# --- phase 2: the installed system works (full mode only) --------------------

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
  group "Checking the installation"

  assert "system user '$OS_USER' created" id "$OS_USER"
  # privileged, and not out of habit: the perimeter is 0750, so the user running
  # this script may not TRAVERSE it. an unprivileged test there does not answer
  # "absent", it answers "permission denied" — and the assertion turns both into
  # the same red.
  #
  # on native runners these passed, which means they passed by a property of the
  # environment and not because the question was well put; on another family the
  # bill arrived. a check must be made with the privileges the question needs, or
  # it measures the permissions of whoever runs it.
  assert "sources in $INSTALL_DIR" sudo test -d "$INSTALL_DIR/odoo"
  assert "virtualenv created" sudo test -x "$INSTALL_DIR/sandbox/bin/python3"
  assert "config generated" sudo test -f "$INSTALL_DIR/odoo${VER_SHORT}.conf"
  assert "systemd unit installed" test -f "$UNIT_FILE"
  assert "service active" systemctl is-active --quiet "$UNIT"
  assert "service enabled" systemctl is-enabled --quiet "$UNIT"
  assert "wkhtmltopdf installed" pkg_installed wkhtmltox

  # the database exists and carries an initialised schema.
  if [ "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" = "1" ]; then
    ok "database '$DB_NAME' created"
  else
    fail "database '$DB_NAME' absent"
  fi
  if [ "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" = "1" ]; then
    ok "PostgreSQL role '$DB_ROLE' created"
  else
    fail "PostgreSQL role '$DB_ROLE' absent"
  fi
  if [ "$(sudo -u postgres psql -tAd "$DB_NAME" -c \
        "select 1 from information_schema.tables where table_name='ir_module_module'" 2>/dev/null)" = "1" ]; then
    ok "Odoo schema initialised (ir_module_module present)"
  else
    fail "Odoo schema not initialised"
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
    200|301|302|303|307) ok "Odoo answers on :$PORT (HTTP $http)" ;;
    *) fail "Odoo does not answer on :$PORT (last code: ${http:-none})"
       sudo journalctl -u "$UNIT" -n 60 --no-pager || true ;;
  esac

  # A-R5-1: the state survives success and is marked finished. that is what
  # makes a later uninstall possible; if this fails, the rollback below would
  # have nothing to consume.
  assert "the uninstall manifest stayed on disk" sudo test -f "$STATE"
  if [ "$(state_json | jq -r '.finished')" = "true" ]; then
    ok "the manifest is marked 'finished'"
  else
    fail "the manifest is NOT marked 'finished': the rollback would read it as an \
interrupted installation"
  fi
  if [ "$(state_json | jq -r '.config.db_name')" = "$DB_NAME" ]; then
    ok "the manifest carries the real configuration (db_name=$DB_NAME)"
  else
    fail "the manifest does not carry the real db_name: the rollback would not know what \
to remove (an A-R4-1 regression)"
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
  group "Checking nginx"

  assert "vhost generated in $VHOST" sudo test -f "$VHOST"
  if [ -n "$VHOST_LINK" ]; then
    assert "site enabled ($VHOST_LINK)" sudo test -L "$VHOST_LINK"
  else
    info "no symlink to check: on this family the vhost already lives in the active dir"
  fi
  assert "the nginx configuration is valid (nginx -t)" sudo nginx -t
  assert "nginx active" systemctl is-active --quiet nginx

  # A-V3-12: the vhost's logs carry the version, not a hardcoded one.
  if sudo grep -q "odoo${VER_SHORT}.access.log" "$VHOST"; then
    ok "the vhost logs follow the installed version (A-V3-12)"
  else
    fail "the vhost logs carry no version: two instances would write over each other"
    sudo grep -n "access_log\|error_log" "$VHOST" || true
  fi

  # A-V3-6: the vhost promises no TLS. a 443 block towards non-existent
  # certificates would fail validation — which above it passes.
  if sudo grep -qE "^\s*listen\s+443|^\s*ssl_certificate" "$VHOST"; then
    fail "the vhost contains TLS directives: the installer does not generate them (A-V3-6)"
  else
    ok "the vhost promises no TLS (that is certbot --nginx's job)"
  fi

  # the default site was moved out of the way, which is what frees port 80.
  if [ "$HAS_DEFAULT_SITE" = "1" ]; then
    refute "the default site was disabled" sudo test -e "$DEFAULT_SITE"
  fi

  # the firewall: port 80 must have been opened (A-V3-7).
  #
  # this is where the token comparison is proved on a real machine. with the old
  # substring check, a similar-looking rule made port 80 look already open: it
  # never entered the delta, was never opened, and nginx stayed unreachable
  # **with no error at all**.
  if [ "$FW_ACTIVE" = "1" ]; then
    if fw_open_ports | grep -qx "80/tcp"; then
      ok "$FW_NAME rule 80/tcp opened (A-V3-7: not confused with 8080/tcp)"
    else
      fail "port 80 was NOT opened on $FW_NAME: nginx stays unreachable"
      fw_open_ports | sed 's/^/    port· /' || true
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
    200|301|302|303|307) ok "Odoo answers through nginx on port 80 (HTTP $http80)" ;;
    *) fail "port 80 does not serve Odoo (last code: ${http80:-none})"
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
  group "Second installation: it must be refused (A-V3-1)"

  sudo cp "$STATE" "$WORK/manifest-before.json"

  set +e
  sudo "$BIN" --config "$ENV_FILE" > "$WORK/second-install.log" 2>&1
  SECOND_RC=$?
  set -e
  echo "second installation exit code: $SECOND_RC"

  if [ "$SECOND_RC" -ne 0 ]; then
    ok "the second installation was refused (exit $SECOND_RC)"
  else
    fail "the second installation was ACCEPTED: the manifest is at risk (A-V3-1)"
  fi

  sudo cp "$STATE" "$WORK/manifest-after.json"
  # the comparison exits non-zero both when the files DIFFER and when it cannot
  # compare them at all. telling the two apart is not pedantry: the tool was
  # missing from one image and this block accused the manifest of having changed
  # when nobody had looked at it. a check that cannot run must say "I could not",
  # never "it went badly" — the blindness-versus-absence distinction again.
  if ! command -v cmp >/dev/null 2>&1; then
    fail "cannot compare the manifest: 'cmp' is not installed \
(the diffutils package). the A-V3-1 check did NOT run"
  elif sudo cmp -s "$WORK/manifest-before.json" "$WORK/manifest-after.json"; then
    ok "the manifest stayed identical byte for byte"
  else
    fail "the manifest changed after a refused installation: \
the instance may no longer be removable (A-V3-1)"
    sudo diff "$WORK/manifest-before.json" "$WORK/manifest-after.json" 2>/dev/null || true
  fi

  # the refusal must come from the MANIFEST, not from a side effect.
  #
  # A-R9-1: this block's first version settled for a non-zero exit and a message
  # naming the rollback command. it failed for the wrong reason — the port check
  # rejected the installation first, because Odoo was listening, and the manifest
  # check was never reached. "free the port" sends the user to stop Odoo, not to
  # uninstall it.
  if grep -q "already registered on this machine" "$WORK/second-install.log"; then
    ok "the refusal comes from the manifest (A-V3-1), not from a side effect"
  else
    fail "the second installation was rejected, but NOT by the manifest check: \
A-V3-1's refusal was never reached (A-R9-1)"
    tail -n 20 "$WORK/second-install.log" || true
  fi

  if grep -q -e 'port .* already in use' "$WORK/second-install.log"; then
    fail "the refusal comes from the port check: that is a CONSEQUENCE of the existing \
installation, not the cause — the user is sent to stop Odoo (A-R9-1)"
  else
    ok "no misleading diagnosis about the port"
  fi

  # all THREE ways out, each asserted on its own: an `-e a -e b` grep passes on
  # any one of them, so it would have stayed green while the message named two.
  # and the one that matters most is the additive one — since instances exist,
  # somebody re-running the installer here usually wants a second one, not to
  # undo or overwrite what is already working.
  for hint in 'rollback' '--force' '--instance'; do
    if grep -q -- "$hint" "$WORK/second-install.log"; then
      ok "the refusal offers '$hint'"
    else
      fail "the refusal does not mention '$hint': the user is left without that way out"
      tail -n 20 "$WORK/second-install.log" || true
    fi
  done

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

group "The run's journal (from the log)"

# the parsing lives in journal.sh, exercised by its self-test in the fast CI: a
# pattern that does not match gives no error, it gives zero results — and zero
# packages to verify looks like a passing check.
sed_out="$WORK/install.txt"
journal_strip_ansi "$WORK/install.out" > "$sed_out"

readonly DEP_STEP='install-system-dependencies'
journal_steps "$sed_out" > "$WORK/steps.txt"
journal_packages "$sed_out" 'packages added by us' "$DEP_STEP" > "$WORK/delta.txt"
journal_packages "$sed_out" 'packages already there, never touched' "$DEP_STEP" \
  > "$WORK/preexisting.txt"

info "completed steps:                $(wc -l < "$WORK/steps.txt")"
info "delta (installed by us):        $(wc -l < "$WORK/delta.txt") packages"
info "pre-existing (never touched):   $(wc -l < "$WORK/preexisting.txt") packages"

if [ ! -s "$WORK/delta.txt" ] && [ "$MODE" = "full" ]; then
  fail "no package in the delta: the journal was not read correctly, and the \
cleanliness checks would pass on nothing"
fi

# in probe mode the installation stops early by construction. without this
# check, a failure at step one would pass every cleanliness check for the wrong
# reason: nothing to clean because nothing was done. these two steps are what the
# probe exists to exercise — that OS's package names (A5.1) and its codename's
# checksum pin (A5.2, A-RT-1).
if [ "$MODE" = "probe" ]; then
  for step in install-system-dependencies install-wkhtmltopdf; do
    if grep -qx "$step" "$WORK/steps.txt"; then
      ok "the probe got past '$step' on this OS"
    else
      fail "the probe did not reach '$step': portability was not verified"
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
echo "rollback exit code: $ROLLBACK_RC"
endgroup

if [ "$ROLLBACK_RC" -eq 0 ]; then
  ok "the rollback finished without errors"
else
  fail "the rollback exited with $ROLLBACK_RC"
fi
# two outcomes are both correct, and telling them apart matters.
#
# "no leftovers" means there was a registered installation and all of it was
# undone. "nothing to undo" means there was nothing to do — the NORMAL case in
# probe mode, where the installation fails and **undoes itself** during the run,
# so by the time we get here the system is already clean and the manifest gone.
# demanding the first would demand that something was still left.
if grep -q "No leftovers" "$WORK/rollback.out"; then
  ok "the rollback declares no leftovers"
elif grep -q "nothing to undo" "$WORK/rollback.out"; then
  if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
    fail "there was nothing to undo, but the installation had succeeded: the uninstall \
manifest disappeared when it should have stayed"
  else
    ok "nothing to undo: the in-process rollback had already cleaned everything"
  fi
else
  fail "the rollback left leftovers (see the report above)"
fi

# --- phase 5: the system is clean again --------------------------------------
#
# the same checks once done by hand on a VM, now automatic. the point is not that
# the command exited zero: it is that nothing of ours is left on the system.

group "Cleanliness check after the rollback"

if [ "$OS_USER_PREEXISTING" = "1" ]; then
  # pre-existing: NOT ours to delete. the anti-drop applied to users, and here
  # the assertion runs the other way round.
  assert "the pre-existing user '$OS_USER' survived the rollback" id "$OS_USER"
else
  refute "the system user '$OS_USER' was removed" id "$OS_USER"
  refute "the group '$OS_USER' was removed" getent group "$OS_USER"
fi
refute "the install directory is gone" test -d "$INSTALL_DIR"
refute "the systemd unit was removed" test -f "$UNIT_FILE"
refute "the service is no longer active" systemctl is-active --quiet "$UNIT"
refute "the manifest was consumed" sudo test -f "$STATE"
refute "wkhtmltopdf was purged" pkg_installed wkhtmltox

# the alternative interpreter goes with everything else (M11).
#
# the half that makes the choice *reversible*: tens of megabytes installed by us
# and never removed would be a leftover inside the perimeter the rollback
# promises to restore — which is why the step that carries it is the one that
# purges its delta and not the one that leaves what it adds.
if [ -n "$PYTHON_PLAN" ]; then
  refute "the interpreter '$PYTHON_PLAN' we installed was removed" \
    pkg_installed "$PYTHON_PLAN"
fi

if pg_reachable; then
  if [ -z "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" ]; then
    ok "database '$DB_NAME' dropped"
  else
    fail "database '$DB_NAME' still present"
  fi
  if [ -z "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" ]; then
    ok "PostgreSQL role '$DB_ROLE' removed"
  else
    fail "PostgreSQL role '$DB_ROLE' still present"
  fi
else
  info "PostgreSQL unreachable: the DB and role checks were skipped"
  info "(expected in MODE=probe, where the service never started)"
fi

# PostgreSQL stays INSTALLED: without the aggressive flag the rollback only
# stops and disables, because those are reversible and a purge is not (D3). that
# it stays is correct behaviour, not a leftover.
if pkg_installed postgresql; then
  ok "PostgreSQL stays installed (correct: purge only with --aggressive-rollback)"
fi

# the customer's nginx config comes back as it was (B-V3-7).
#
# A-V3-5's half that counts. the default site is **somebody else's pre-existing
# configuration**: removing it to free port 80 is lawful, not putting it back is
# not. until this phase no real run had ever checked it.
if [ "$WITH_NGINX" = "true" ]; then
  refute "the vhost was removed" sudo test -f "$VHOST"
  if [ -n "$VHOST_LINK" ]; then
    refute "the site was disabled" sudo test -e "$VHOST_LINK"
  fi

  # on that family the default site is not a separate file: there is no return
  # to verify, and inventing the assertion would give a green for the wrong
  # reason.
  if [ "$HAS_DEFAULT_SITE" = "1" ]; then
  case "$DEFAULT_SITE_BEFORE" in
    absent)
      refute "no default site invented by the rollback" sudo test -e "$DEFAULT_SITE"
      ;;
    symlink:*)
      expected="${DEFAULT_SITE_BEFORE#symlink:}"
      if [ -L "$DEFAULT_SITE" ] && [ "$(readlink "$DEFAULT_SITE")" = "$expected" ]; then
        ok "the default site came back to its original target ($expected)"
      else
        fail "the default site was NOT restored as it was: expected a symlink to $expected, \
found $( [ -e "$DEFAULT_SITE" ] && ls -ld "$DEFAULT_SITE" || echo absent )"
      fi
      ;;
    file)
      if [ -f "$DEFAULT_SITE" ] && [ ! -L "$DEFAULT_SITE" ]; then
        ok "the default site (a regular file) was put back"
      else
        fail "a default site that was a FILE did not come back as one: the configuration \
loss A-V3-5 describes"
      fi
      ;;
  esac
  fi

  # after the rollback: close what we opened, and **only** that. a customer's
  # pre-existing rule is never touched — the package delta's rule, applied to the
  # firewall.
  if [ "$FW_ACTIVE" = "1" ]; then
    if fw_open_ports | grep -qx "80/tcp"; then
      fail "the 80/tcp rule we opened stayed after the rollback ($FW_NAME)"
    else
      ok "the $FW_NAME 80/tcp rule was closed again"
    fi
    if fw_open_ports | grep -qx "8080/tcp"; then
      ok "the pre-existing 8080/tcp rule is intact ($FW_NAME)"
    else
      fail "the rollback removed a $FW_NAME rule it had not opened"
    fi
  fi

  # nginx survives and stays serviceable: the rollback must not leave the
  # customer's service with a broken config (A1.4).
  if pkg_installed nginx || pkg_installed nginx-core || pkg_installed nginx-full; then
    ok "nginx stays installed (purge only with --aggressive-rollback)"
  fi
  assert "the nginx configuration is valid after the rollback too" sudo nginx -t
fi

# the package delta: purged in full.
delta_left=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if pkg_installed "$pkg"; then
    fail "a delta package is still installed: $pkg"
    delta_left=$((delta_left + 1))
  fi
done < "$WORK/delta.txt"
# an `if`, not a short-circuit: under `set -e` a false test as the last command
# of an `&&` list would exit the script before the final report.
if [ "$delta_left" -eq 0 ]; then
  ok "every delta package was purged"
fi

# the pre-existing ones: never touched. the surgical promise from the other
# side — a rollback that purges too much is as bad as one that purges too little.
preexisting_lost=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if ! pkg_installed "$pkg"; then
    fail "a PRE-EXISTING package was removed by the rollback: $pkg"
    preexisting_lost=$((preexisting_lost + 1))
  fi
done < "$WORK/preexisting.txt"
if [ "$preexisting_lost" -eq 0 ]; then
  ok "no pre-existing package was touched"
fi

# the same promise **without bookkeeping**: the set of installed packages before
# is compared with the set now. it does not depend on what we recorded, so it
# cannot pass for the wrong reason — the difference between "the packages we said
# we added are gone" and "nothing that was there is gone".
pkgs_installed_now > "$WORK/pkgs-after.txt"
if lost="$(comm -23 "$WORK/pkgs-before.txt" "$WORK/pkgs-after.txt")" && [ -z "$lost" ]; then
  ok "no package present before the installation was removed"
else
  fail "the rollback removed packages that were already there: $(echo "$lost" | tr '\n' ' ')"
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
  ok "$ODOO_HOME was pre-existing: the rollback must not remove it (nothing asserted)"
elif [ -d "$ODOO_HOME" ]; then
  leftovers="$(sudo ls -A "$ODOO_HOME" | tr '\n' ' ' || true)"
  fail "$ODOO_HOME survived the rollback (contents: ${leftovers:-empty}) — A-V3-2"
else
  ok "$ODOO_HOME no longer exists: the perimeter is back as it was"
fi

endgroup

# --- outcome -----------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== INTEGRATION OK ($MODE): installation and rollback verified on the real system ==="
  exit 0
fi
echo "=== INTEGRATION FAILED ($MODE): $FAILURES checks did not pass ==="
echo "Failed checks:"
for check in "${FAILED_CHECKS[@]}"; do
  echo "  ✖  $check"
  # an error annotation shows at the top of the run: visible without opening
  # anything, which is the point.
  echo "::error::$check"
done
echo "The installer's log:"
sudo tail -n 100 /var/log/invok.log 2>/dev/null || echo "(no log)"
exit 1
