//! [`WriteControlScript`]: installs the `odoo` helper command.
//!
//! the artifacts belong to **`SUDO_USER`**, not to `odoo` nor to root, and are
//! installed **for that user only** rather than globally — a security choice
//! from the original Bash, preserved.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::SystemOps;

/// the control script's template; placeholders are substituted by hand.
const CONTROL_SCRIPT_TEMPLATE: &str = r#"#!/usr/bin/env bash
#
# invok's helper for ONE Odoo instance. It drives that instance and no other,
# deliberately: a machine can carry several — each with its own service, user,
# database and port — and a helper that started or stopped somebody else's would
# take one customer offline to fix another one's problem. What it does do is
# SHOW the others, with the command that drives each: reading, never touching.

set -euo pipefail

SERVICE_NAME="__SERVICE__"
ODOO_OS_USER="__OSUSER__"
COMMAND_NAME="__COMMAND__"

# how long the question asked when leaving `dev` waits for an answer, in
# seconds. It has to end: a question nobody answers holds the service down for
# exactly as long as it is left on screen.
DEV_ANSWER_TIMEOUT=30

usage() {
  cat <<USAGE
${COMMAND_NAME} — controls the Odoo instance served by '${SERVICE_NAME}'

Usage: ${COMMAND_NAME} {start|stop|restart|status|list|logs [N]|dev}

  start     start this instance's service
  stop      stop it
  restart   restart it — after changing its configuration file
  status    its state, followed by every Odoo service on this machine
  list      just that listing: what is installed here and what is up
  logs      follow its log, from the last N lines (default 100).
            Ctrl-C stops reading; the service keeps running
  dev       stop it and open a shell as '${ODOO_OS_USER}', to run odoo-bin by
            hand. On the way out the service is put back the way you found it:
            you are asked first, and the answer nobody types is always "as it
            was"

Only this instance is ever touched. 'list' (and 'status', which prints it too)
shows the others, with the command that drives each one.
USAGE
}

# every Odoo service on this machine, ours marked.
#
# asked of systemd rather than of the manifests, and that is the point: the
# question here is "what is running NOW", which no manifest records — the same
# distinction the installer makes when it checks a port. A manifest says who
# reserved something, systemd says who is holding it.
#
# TWO commands, and the reason is worth keeping. `list-units` was the obvious
# one and it is the WRONG source: a service that is stopped gets unloaded, so it
# vanishes from that listing even with `--all` — which is precisely the instance
# somebody running `status` is looking for. The unit FILES are the record of
# what is installed; `is-active` answers, per unit, what is up.
list_instances() {
  echo
  echo "Odoo services on this machine:"
  local unit state marker command_for
  while read -r unit _rest; do
    [ -n "${unit:-}" ] || continue
    state="$(systemctl is-active "${unit}" 2>/dev/null || true)"
    [ -n "${state}" ] || state="unknown"
    # `if` rather than `[ … ] && …`: under `set -e` the second form has a rule
    # subtle enough to be worth not relying on in generated code.
    marker="  "
    command_for=""
    if [ "${unit}" = "${SERVICE_NAME}.service" ]; then
      marker="->"
      # the helper's name is the instance's: `odoo` for the historical one,
      # `odoo-<name>` for the others. The unit carries the version instead, so
      # only ours can be named with certainty.
      command_for="   (this one: ${COMMAND_NAME})"
    fi
    printf '%s %-32s %s%s\n' "${marker}" "${unit}" "${state}" "${command_for}"
  done < <(systemctl list-unit-files --no-pager --no-legend 'odoo*.service' 2>/dev/null || true)
  echo
  echo "Each instance has a helper of its own: 'odoo' for the historical one,"
  echo "'odoo-<name>' for a named one. Use that helper to start or stop it —"
  echo "this one only drives '${SERVICE_NAME}'."
}

# what `dev` owes you on the way out: the machine you walked in on.
#
# it is reached from a trap on EXIT **and** on HUP/INT/TERM, and that is the
# whole point. The reason this exists is somebody who closes the window and
# forgets — and a closed window is SIGHUP, with nobody left to answer anything.
# So the question is an OFFER, never the mechanism: with no terminal, or when
# the wait runs out, the same rule is applied in silence.
#
# The rule is the installer's own, the one the rollback follows: put back the
# state you found. Which is also why the default is DERIVED from that state
# instead of being a fixed "yes" — with a service found stopped, a fixed yes
# would make Enter start what somebody switched off on purpose, and Enter is
# precisely what a distracted person presses.
restore_service() {
  local rc=$?
  # once only: the signal traps and the EXIT trap would otherwise both fire.
  trap - EXIT HUP INT TERM

  # from here on the shell is BEST-EFFORT, and it is not a shortcut: this is an
  # undo, and the installer's third invariant says an undo reports and carries
  # on instead of dying. The shell needs it spelled out because `set -e` makes
  # the opposite the default — and that default cost the whole feature once.
  #
  # A closed window is the scenario this exists for, and it is also the one in
  # which every write from here fails: the pty master is gone, so `echo` gets
  # EIO. Under `set -e` that killed the restore before it reached the service,
  # leaving a customer's Odoo down — the recovery path defeated by its own
  # report, exactly where nobody is watching. The action must never be hostage
  # to the telling of it; the one thing that MUST be noticed still is, by hand,
  # where the service is started.
  set +e

  local now prompt default answer
  # `is-active` is asked for its OUTPUT: it exits non-zero for a service that
  # is simply not running, which is an answer and not a failure.
  now="$(systemctl is-active "${SERVICE_NAME}" 2>/dev/null || true)"

  # up when you leave: either you never let it stop, or you started it again
  # yourself from inside the shell. Both are the state you want, so there is
  # nothing to ask and nothing to do.
  if [ "${now}" = "active" ]; then
    exit "${rc}"
  fi

  echo
  if [ "${dev_state_on_entry:-}" = "active" ]; then
    prompt="Restart '${SERVICE_NAME}'? [Y/n] "
    default="y"
  else
    echo "'${SERVICE_NAME}' was already stopped when you entered dev."
    prompt="Start it now? [y/N] "
    default="n"
  fi

  answer="${default}"
  if [ -t 0 ]; then
    printf '%s' "${prompt}"
    # a read that fails is a timeout or a terminal that went away: both mean
    # nobody answered, which is what the default is for.
    if ! read -r -t "${DEV_ANSWER_TIMEOUT}" answer; then
      echo
      answer="${default}"
    fi
    [ -n "${answer}" ] || answer="${default}"
  fi

  case "${answer}" in
    [yY]*)
      if ! sudo systemctl start "${SERVICE_NAME}"; then
        echo "'${SERVICE_NAME}' did not come back up. Look at: ${COMMAND_NAME} logs" >&2
      fi
      ;;
    *)
      echo "'${SERVICE_NAME}' is left STOPPED. Start it with: ${COMMAND_NAME} start"
      ;;
  esac
  exit "${rc}"
}

case "${1:-}" in
  start)
    sudo systemctl start "${SERVICE_NAME}"
    ;;
  stop)
    sudo systemctl stop "${SERVICE_NAME}"
    ;;
  restart)
    sudo systemctl restart "${SERVICE_NAME}"
    ;;
  dev)
    # the state is read BEFORE anything changes it, and the trap is armed
    # before the stop rather than after: between those two lines is exactly
    # where a kill would leave the service down with nobody remembering that
    # it had been up.
    dev_state_on_entry="$(systemctl is-active "${SERVICE_NAME}" 2>/dev/null || true)"
    trap restore_service EXIT HUP INT TERM
    echo "stopping '${SERVICE_NAME}' — the other instances on this machine are left alone."
    sudo systemctl stop "${SERVICE_NAME}"
    sudo su - "${ODOO_OS_USER}" -s /bin/bash
    ;;
  list)
    # the listing on its own, without this instance's `systemctl status` above
    # it. Every helper answers it, and answers the same thing: "what is on this
    # machine, what is up, and which command drives each". From there you invoke
    # the helper of the instance you actually meant — the action stays explicit,
    # and no helper acquires the power to stop somebody else's service.
    list_instances
    ;;
  logs)
    # this instance's journal and nobody else's, like every other verb here.
    #
    # it follows, because that is what a log is opened for while something is
    # wrong; `-n` gives the lines before the tail. Ctrl-C ends `journalctl` and
    # with it this script — the service is untouched, which the usage says out
    # loud: right after `dev`, somebody may reasonably fear that Ctrl-C stops
    # Odoo.
    sudo journalctl -u "${SERVICE_NAME}" -n "${2:-100}" -f
    ;;
  status)
    # systemctl exits non-zero for a service that is not running, and under
    # `set -e` that would take the listing with it — precisely when it is most
    # wanted. The code is kept and handed on at the end.
    rc=0
    sudo systemctl status "${SERVICE_NAME}" --no-pager || rc=$?
    list_instances
    exit "${rc}"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
"#;

const SCRIPT_MODE: u32 = 0o755;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ControlScriptSnapshot {
    pub script: PreState,
    pub symlink: PreState,
    /// did the directories exist before us? only ours are removed.
    pub scripts_dir_existed: bool,
    pub localbin_dir_existed: bool,
    /// backup of a **pre-existing** script, when there was one to set aside
    /// before rewriting (A-V3-9).
    #[serde(default)]
    pub script_backup: Option<String>,
}

pub struct WriteControlScript {
    ops: Box<dyn SystemOps>,
    snap: ControlScriptSnapshot,
}

impl WriteControlScript {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: ControlScriptSnapshot::default(),
        }
    }

    /// resolves `(SUDO_USER, home)`.
    ///
    /// # errors
    ///
    /// [`StepError::Precondition`] when `SUDO_USER` is absent or has no home.
    fn user_and_home(&self, ctx: &Context) -> Result<(String, std::path::PathBuf), StepError> {
        let user = ctx.sudo_user.clone().ok_or_else(|| {
            StepError::Precondition(
                "SUDO_USER is unset: the installer must run through sudo".to_string(),
            )
        })?;
        let home = self.ops.getent_home(&user)?.ok_or_else(|| {
            StepError::Precondition(format!("cannot determine the home of '{user}'"))
        })?;
        Ok((user, std::path::PathBuf::from(home)))
    }
}

/// builds the control script's contents.
///
/// `command` is the name the helper is invoked by, and it goes into the usage
/// line: a helper called `odoo-cliente-x` printing "Usage: odoo" would send the
/// reader to the wrong instance's command.
pub fn control_script_content(service: &str, os_user: &str, command: &str) -> String {
    CONTROL_SCRIPT_TEMPLATE
        .replace("__SERVICE__", service)
        .replace("__OSUSER__", os_user)
        .replace("__COMMAND__", command)
}

/// where the helper's four artifacts live, given the invoking user's home and
/// this instance's command name.
///
/// the name is `odoo` for the unnamed instance — unchanged — and
/// `odoo-<instance>` otherwise. one helper per instance rather than one helper
/// taking the instance as an argument, and the reason is not ergonomics: a
/// single shared file would be **rewritten by every installation**, which is
/// A-V3-9 all over again, one instance driving another's service.
fn paths(
    home: &std::path::Path,
    command: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let scripts_dir = home.join(".scripts");
    let script = scripts_dir.join(format!("{command}.sh"));
    let localbin = home.join(".local").join("bin");
    let symlink = localbin.join(command);
    (scripts_dir, script, localbin, symlink)
}

impl Step for WriteControlScript {
    fn name(&self) -> &str {
        "write-control-script"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let (_user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home, &ctx.qualified_name());

        self.snap.script = if self.ops.path_exists(&script) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.symlink = if self.ops.symlink_exists(&symlink) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.scripts_dir_existed = self.ops.path_exists(&scripts_dir);
        self.snap.localbin_dir_existed = self.ops.path_exists(&localbin);
        info!(script = ?self.snap.script, symlink = ?self.snap.symlink, "snapshot: write-control-script");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let (user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home, &ctx.qualified_name());

        if ctx.dry_run {
            info!("run (dry run): would write odoo.sh and the symlink for user {user}");
            return Ok(());
        }

        // directories created as the installing user.
        self.ops.mkdir_p_as_user(&user, &scripts_dir)?;
        self.ops.mkdir_p_as_user(&user, &localbin)?;

        // the script is **always** rewritten (A-V3-9): its contents are ours
        // and carry the service name, so skipping it because "it exists" left
        // the helper driving the **old** service after reinstalling another
        // version — discovered only when `odoo restart` did the wrong thing.
        //
        // the `PreState` here does not protect somebody else's contents but the
        // *decision to remove it in the undo*. an existing file is still set
        // aside rather than destroyed, and the undo puts it back — the same
        // treatment as the nginx vhost and `odoo.conf`.
        if self.snap.script == PreState::Preexisting && self.snap.script_backup.is_none() {
            let backup = format!("{}.bak.{}", script.display(), unix_timestamp());
            self.ops.copy_file(&script, std::path::Path::new(&backup))?;
            self.snap.script_backup = Some(backup.clone());
            warn!(backup = %backup, "run: the control script existed, a backup was made");
        }
        let service = ctx.artifact_base();
        let content = control_script_content(&service, &ctx.odoo_user, &ctx.qualified_name());
        self.ops.write_private_file(&script, &content)?;
        // ours from here (A-V3-24), before the mode that can fail.
        if self.snap.script == PreState::Untracked {
            self.snap.script = PreState::CreatedByUs;
        }
        self.ops.chmod(&script, SCRIPT_MODE)?;

        // the symlink, unless it is already ours.
        if self.snap.symlink != PreState::Preexisting {
            self.ops.create_symlink(&script, &symlink)?;
            self.snap.symlink = PreState::CreatedByUs;
        }

        // everything belongs to SUDO_USER, not to `odoo` nor root.
        self.ops.chown_to_user(&script, &user)?;
        self.ops.chown_to_user(&symlink, &user)?;
        self.ops.chown_to_user(&scripts_dir, &user)?;
        self.ops.chown_to_user(&localbin, &user)?;

        info!(script = %script.display(), "run: control script installed for {user}");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        let (_user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home, &ctx.qualified_name());

        if ctx.dry_run {
            info!("undo (dry run): would remove the script and the symlink (and the dirs if we made them)");
            return Ok(());
        }

        // remove only what we created.
        if self.snap.symlink == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_symlink(&symlink) {
                warn!(error = %e, "undo: removing the symlink failed, proceeding (best-effort)");
            }
        }
        match (&self.snap.script, &self.snap.script_backup) {
            // ours: removed.
            (PreState::CreatedByUs, _) => {
                if let Err(e) = self.ops.remove_file(&script) {
                    warn!(error = %e, "undo: removing the script failed, proceeding (best-effort)");
                }
            }
            // it was there and we rewrote it: the original comes back.
            (PreState::Preexisting, Some(backup)) => {
                let backup_path = std::path::Path::new(backup);
                if self.ops.path_exists(backup_path) {
                    if let Err(e) = self.ops.move_file(backup_path, &script) {
                        warn!(error = %e, "undo: restoring the control script failed, proceeding (best-effort)");
                    } else {
                        info!("undo: the pre-existing control script was restored from the backup");
                    }
                } else {
                    warn!(backup = %backup, "undo: the control script's backup was not found");
                }
            }
            _ => {}
        }

        // directories go only if we created them and they are empty.
        if !self.snap.scripts_dir_existed {
            remove_if_empty(self.ops.as_ref(), &scripts_dir);
        }
        if !self.snap.localbin_dir_existed {
            remove_if_empty(self.ops.as_ref(), &localbin);
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}

/// removes a directory only if empty, best-effort.
fn remove_if_empty(ops: &dyn SystemOps, dir: &std::path::Path) {
    match ops.dir_is_empty(dir) {
        Ok(true) => {
            if let Err(e) = ops.rmdir(dir) {
                warn!(dir = %dir.display(), error = %e, "undo: rmdir failed, proceeding (best-effort)");
            }
        }
        Ok(false) => info!(dir = %dir.display(), "undo: the directory is not empty, leaving it"),
        Err(e) => warn!(dir = %dir.display(), error = %e, "undo: inspecting the directory failed"),
    }
}
