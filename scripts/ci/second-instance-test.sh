#!/usr/bin/env bash
# =============================================================================
# Two instances on one machine, installed and removed one at a time (phase I4).
#
# a file rather than inline YAML, and for the reason A-R8-1-ter cost a day: a
# script that lives in the workflow can only be exercised by the workflow, so
# what gets tried on a VM is a *transcription* of it — and the transcription is
# where the difference hides. this way the CI and the VM run the same bytes.
#
# it takes no argument and reads three variables, all with the values the CI
# uses:
#
#   INVOK       the binary (a path on the runner, a command on a VM)
#   BASE_ENV    the first, historical, unnamed instance
#   NAMED_ENV   the second one — it must NOT name user, role or database:
#               those are what `--instance` derives, and an explicit value
#               would beat them (see tests/ci_config.rs)
#
# DESTRUCTIVE: it installs, creates users, touches PostgreSQL and systemd. only
# on a CI runner, a container, or a VM that can be thrown away.
# =============================================================================
set -euo pipefail

INVOK="${INVOK:-./target/release/invok}"
BASE_ENV="${BASE_ENV:-configs/ci.env}"
NAMED_ENV="${NAMED_ENV:-configs/ci-instance.env}"
INSTANCE="${INSTANCE:-cliente-x}"
WORK="${WORK:-/tmp}"

failed=0
say() { if [ "$2" = "ok" ]; then echo "  ✔ $1"; else echo "::error::$1"; failed=1; fi; }
check() { # description, then a command that must FAIL
  if eval "$2" >/dev/null 2>&1; then echo "::error::$1"; failed=1; else echo "  ✔ $1"; fi
}
packages() { dpkg-query -W -f='${Package}\n' | sort; }

phase() { echo; echo "=== $1"; }

phase "the machine before anything"
packages > "$WORK"/packages-virgin
echo "packages: $(wc -l < "$WORK"/packages-virgin)"
test ! -d /opt/odoo || { echo "::error::/opt/odoo already exists"; exit 1; }

phase "first instance — the historical one, unnamed"
sudo "$INVOK" --config "$BASE_ENV"
systemctl is-active odoo18 >/dev/null || { echo "::error::the first instance is not running"; exit 1; }
curl -fsS -o /dev/null -w 'first instance: HTTP %{http_code}\n' http://localhost:8069/ \
  || { echo "::error::the first instance does not answer"; exit 1; }
# the precondition A-V6-9 is about: the root belongs to the first instance and
# is 0750, so a second user cannot walk through it.
stat -c '/opt/odoo is %U:%G %a' /opt/odoo
[ "$(stat -c '%a' /opt/odoo)" = "750" ] \
  || { echo "::error::the root is not 0750: the scenario this exists for is not set up"; exit 1; }
packages > "$WORK"/packages-after-first

phase "second instance — named, beside the first"
sudo "$INVOK" --config "$NAMED_ENV" --instance "$INSTANCE"
packages > "$WORK"/packages-after-second

phase "both up, and sharing nothing they should not"
# --- both alive ---------------------------------------------------
systemctl is-active odoo18        >/dev/null && say "the first instance is running" ok  || say "the first instance stopped" no
systemctl is-active odoo-$INSTANCE >/dev/null && say "the second instance is running" ok || say "the second instance is not running" no
curl -fsS -o /dev/null http://localhost:8069/ && say "8069 answers" ok || say "8069 does not answer" no
curl -fsS -o /dev/null http://localhost:8169/ && say "8169 answers" ok || say "8169 does not answer" no

# --- the root was widened, and by one bit only (A-V6-9) -----------
mode=$(stat -c '%a' /opt/odoo)
echo "  /opt/odoo: $(stat -c '%U:%G %a' /opt/odoo)"
[ "$mode" = "751" ] && say "the shared root was widened to 0751 (walk through, not list)" ok \
                    || say "the shared root is $mode, not 0751" no
sudo -n -u odoo-$INSTANCE test -x /opt/odoo \
  && say "the second instance's user can traverse the root" ok \
  || say "the second instance's user cannot reach its own home" no

# --- the two configurations claim different ports (I3) ------------
echo "--- ports ---"
sudo grep -HE '^(http_port|gevent_port)' /opt/odoo/odoo18/odoo18.conf /opt/odoo/odoo-$INSTANCE/odoo-$INSTANCE.conf
sudo grep -q '^gevent_port = 8072' /opt/odoo/odoo18/odoo18.conf \
  && say "the first keeps 8072" ok || say "the first instance's gevent port changed" no
sudo grep -q '^gevent_port = 8172' /opt/odoo/odoo-$INSTANCE/odoo-$INSTANCE.conf \
  && say "the second derived 8172 from its 8169" ok \
  || say "the second instance did not derive its gevent port" no

# --- separate artifacts -------------------------------------------
id odoo-$INSTANCE >/dev/null 2>&1 && say "the second instance has its own system user" ok || say "no odoo-$INSTANCE user" no
test -f /etc/systemd/system/odoo-$INSTANCE.service && say "its own unit" ok || say "no unit of its own" no
sudo -u postgres psql -tAc "select 1 from pg_database where datname='odoo-$INSTANCE'" | grep -q 1 \
  && say "its own database" ok || say "no database of its own" no
sudo -u postgres psql -tAc "select 1 from pg_database where datname='citest'" | grep -q 1 \
  && say "the first instance's database is untouched" ok || say "the first instance's database is gone" no

# the filestore is the dangerous one: attachments are per instance
# only because the home is (§ 4.1 of the multi-instance register).
test -d /opt/odoo/.local/share/Odoo               && say "the first instance's filestore is where it always was" ok || say "the first filestore moved" no
test -d /opt/odoo/odoo-$INSTANCE/.local/share/Odoo && say "the second instance's filestore is its own" ok || say "the second instance has no filestore of its own" no

# --- one manifest each --------------------------------------------
echo "--- invok list ---"; sudo "$INVOK" list
sudo test -f /var/lib/invok/state.json                       && say "the historical manifest is where it always was" ok || say "the unnamed manifest moved" no
sudo test -f /var/lib/invok/instances/"$INSTANCE".json         && say "the named instance has a manifest of its own" ok || say "no manifest for $INSTANCE" no

# --- the delta: the second instance installed nothing new ----------
if diff -q "$WORK"/packages-after-first "$WORK"/packages-after-second >/dev/null; then
  say "the second instance added no package: it found everything already there" ok
else
  echo "--- packages the second instance added ---"
  diff "$WORK"/packages-after-first "$WORK"/packages-after-second || true
  say "the second instance changed the package set" no
fi

# the crossed comparison of A-V6-3, which nothing else exercises for real: 8072
# is nobody's HTTP port and — with `workers = 0` — nothing is listening on it
# either. only the manifests know it is taken.
phase "the helper of each instance sees the machine, and drives only itself"
# the helper lives in the INVOKING user's home, one per instance, and until now
# nothing exercised it: `status` used to show its own service and nothing else,
# so on a machine with two you could not tell which one you were driving.
HELPER_HOME="$(getent passwd "${SUDO_USER:-$(id -un)}" | cut -d: -f6)"
for helper in odoo "odoo-$INSTANCE"; do
  test -x "$HELPER_HOME/.scripts/$helper.sh" \
    && say "the helper '$helper' was installed" ok \
    || say "no helper '$helper' in $HELPER_HOME/.scripts" no
done

"$HELPER_HOME/.scripts/odoo.sh" status > "$WORK"/helper-status.log 2>&1 || true
cat "$WORK"/helper-status.log
grep -q "odoo18.service" "$WORK"/helper-status.log \
  && say "the helper lists the historical instance" ok || say "odoo18 is missing from the listing" no
grep -q "odoo-$INSTANCE.service" "$WORK"/helper-status.log \
  && say "and the named one beside it" ok || say "the named instance is missing from the listing" no
grep -qE "^-> odoo18\.service .*\(this one: odoo\)" "$WORK"/helper-status.log \
  && say "and it marks which one it drives" ok || say "the listing does not mark this instance" no

# the isolation, measured rather than assumed: this helper must not be able to
# stop the other instance.
"$HELPER_HOME/.scripts/odoo.sh" restart
systemctl is-active odoo-"$INSTANCE" >/dev/null \
  && say "restarting one instance left the other running" ok \
  || say "the helper of one instance touched the other's service" no

phase "a third instance on a longpolling port is refused"
set +e
sudo "$INVOK" --config "$NAMED_ENV" --instance terza --port 8072 > "$WORK"/refuse.log 2>&1
rc=$?
set -e
cat "$WORK"/refuse.log
[ "$rc" -ne 0 ] || { echo "::error::the installation should have been refused"; exit 1; }
grep -q "8072" "$WORK"/refuse.log || { echo "::error::the refusal does not name the port"; exit 1; }
grep -qi "already claims" "$WORK"/refuse.log \
  || { echo "::error::the refusal does not say another instance holds it"; exit 1; }
test ! -d /opt/odoo/odoo-terza || { echo "::error::it created something before refusing"; exit 1; }
sudo test ! -f /var/lib/invok/instances/terza.json \
  || { echo "::error::it left a manifest behind"; exit 1; }
echo "✔ a port claimed by another manifest is refused, with nothing listening on it"

phase "rollback of the SECOND — the first must not notice"
sudo "$INVOK" rollback --instance "$INSTANCE" --yes | tee "$WORK"/rollback-second.log

# --- what must survive, which matters more than what must go ------
systemctl is-active odoo18 >/dev/null && say "the first instance is still running" ok || say "the first instance was stopped" no
curl -fsS -o /dev/null http://localhost:8069/ && say "and still answers" ok || say "the first instance stopped answering" no
id odoo >/dev/null 2>&1 && say "its user is intact" ok || say "its user was removed" no
test -d /opt/odoo/odoo18 && say "its install dir is intact" ok || say "its install dir went" no
test -d /opt/odoo/.local/share/Odoo && say "its filestore is intact" ok || say "its filestore went" no
sudo -u postgres psql -tAc "select 1 from pg_database where datname='citest'" | grep -q 1 \
  && say "its database is intact" ok || say "its database was dropped" no

# the widening comes off: only a NAMED instance ever needed it, and
# the one left owns the root (A-V6-9, and the reason Context carries
# the other instances' names rather than a flag).
mode=$(stat -c '%a' /opt/odoo)
echo "  /opt/odoo: $(stat -c '%U:%G %a' /opt/odoo)"
[ "$mode" = "750" ] && say "the root is back to the 0750 it had" ok \
                    || say "the root stayed at $mode: the widening was not taken back" no

# --- what must go --------------------------------------------------
id odoo-$INSTANCE >/dev/null 2>&1 && say "the second instance's user is still there" no || say "its user is gone" ok
test -f /etc/systemd/system/odoo-$INSTANCE.service && say "its unit is still there" no || say "its unit is gone" ok
test -d /opt/odoo/odoo-$INSTANCE && say "its home is still there" no || say "its home is gone" ok
if sudo -u postgres psql -tAc "select 1 from pg_database where datname='odoo-$INSTANCE'" | grep -q 1; then
  say "its database is still there" no
else
  say "its database is gone" ok
fi

# --- the shared artifacts stay, and the report says so -------------
grep -q "left in place" "$WORK"/rollback-second.log && say "the report names what it left shared" ok \
                                            || say "the report does not mention the shared artifacts" no
# A-V6-11: with another instance installed, a surviving /opt/odoo is
# the rule working — it must NOT be reported as a leftover.
grep -q "still exists" "$WORK"/rollback-second.log && say "the report calls the shared root a leftover" no \
                                           || say "the shared root is not reported as a residue" ok

packages > "$WORK"/packages-after-second-rollback
if diff -q "$WORK"/packages-after-second "$WORK"/packages-after-second-rollback >/dev/null; then
  say "not one package was purged: they are the first instance's" ok
else
  diff "$WORK"/packages-after-second "$WORK"/packages-after-second-rollback || true
  say "the rollback of the second instance purged packages the first one uses" no
fi

echo "--- invok list ---"; sudo "$INVOK" list | tee "$WORK"/list.log
grep -qE "^default .*installed" "$WORK"/list.log && say "the first instance is still listed as installed" ok || say "the first instance is not listed as installed" no
grep -qE "^$INSTANCE .*shared only"  "$WORK"/list.log && say "the second is a tombstone: shared only" ok || say "the tombstone is not reported as such" no

phase "rollback of everything — the machine as it was"
sudo "$INVOK" rollback --all --yes | tee "$WORK"/rollback-all.log

check "/opt/odoo is gone"                 "test -d /opt/odoo"
check "the bookkeeping directory is gone" "test -d /var/lib/invok"
check "the odoo user is gone"             "id odoo"
check "the odoo-$INSTANCE user is gone"   "id odoo-$INSTANCE"
check "the first unit is gone"            "test -f /etc/systemd/system/odoo18.service"
check "the second unit is gone"           "test -f /etc/systemd/system/odoo-$INSTANCE.service"

# PostgreSQL was ours to start, so the rollback stopped it again: a
# failing query then proves nothing. saying so beats a tick that
# looked at nothing.
if sudo -u postgres psql -tAc 'select 1' >/dev/null 2>&1; then
  check "the citest database is gone"          "sudo -u postgres psql -tAc \"select 1 from pg_database where datname='citest'\" | grep -q 1"
  check "the odoo-$INSTANCE database is gone"   "sudo -u postgres psql -tAc \"select 1 from pg_database where datname='odoo-$INSTANCE'\" | grep -q 1"
else
  echo "  · PostgreSQL unreachable (stopped again by the rollback): the database checks did not run"
fi

# the assertion that depends on nothing we recorded ourselves — and it is
# **one-directional**, on purpose: nothing that was there before may be gone.
#
# equality would be wrong, and the first run of this script asserted it and went
# red for the right system doing the right thing: the rollback is surgical, so
# it purges the delta and leaves what the delta pulled in — PostgreSQL (stopping
# is reversible, purging is not), wkhtmltopdf's system dependencies
# (`fontconfig`, `libxrender1`, `xfonts-*`), and every transitive dependency of
# the dev packages. that the delta itself is purged is the `native` job's
# business, on one instance; here what matters is that removing two instances
# took nothing that was not ours.
packages > "$WORK"/packages-end
if lost="$(comm -23 "$WORK"/packages-virgin "$WORK"/packages-end)" && [ -z "$lost" ]; then
  echo "  ✔ no package that was there before the two instances is gone"
else
  echo "::error::the rollback removed packages that were already there: $(echo "$lost" | tr '\n' ' ')"
  failed=1
fi

echo "--- invok list ---"; sudo "$INVOK" list

# last, and after **every** assertion: the first version of this script printed
# its success line from inside the last phase, so a failed check still ended
# with a tick. a summary that can lie is worse than no summary.
[ "$failed" -eq 0 ] || {
  echo
  echo "::error::the two-instance scenario failed: see the assertions above"
  exit 1
}
echo
echo "✔ two instances installed side by side, removed one at a time, and the machine \
is back to what it was"
