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

set -euo pipefail

SERVICE_NAME="__SERVICE__"
ODOO_OS_USER="__OSUSER__"
usage() {
  echo "Usage: __COMMAND__ {start|stop|restart|dev|status}"
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
    sudo systemctl stop "${SERVICE_NAME}"
    sudo su - "${ODOO_OS_USER}" -s /bin/bash
    ;;
  status)
    sudo systemctl status "${SERVICE_NAME}"
    ;;
  *)
    usage
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
        self.ops.chmod(&script, SCRIPT_MODE)?;
        if self.snap.script == PreState::Untracked {
            self.snap.script = PreState::CreatedByUs;
        }

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
