//! [`PrepareOptRoot`]: the first real step — creates `/opt/odoo` when missing.
//!
//! the simplest possible mutation, chosen as the reference model for the steps
//! that follow: a full `snapshot → run → undo` cycle with all three `PreState`s
//! handled.
//!
//! in Bash this `mkdir` hid inside `check_disk` (issue C4: a check that
//! mutates). here the check only measures, and creation is this reversible
//! step.
//!
//! **ordering:** the `odoo` user usually does not exist yet, so the directory
//! is created **owned root**; the `chown` happens in the user-creation step.
//!
//! # if the user already exists, the handover happens here (A-V3-4)
//!
//! `owned root` is not the home's *right* state but a **waiting** one, and it
//! only makes sense while the user is absent. with the user already there,
//! leaving it root-owned broke the installation three steps later:
//! `CreateOdooUser` sees a `Preexisting` user and skips the `chown`
//! deliberately, and `SetupCacheDir` then hits *Permission denied* with an
//! error that says nothing about the cause.
//!
//! the handover belongs **here** because here is where the information is: this
//! step knows it created the directory. `CreateOdooUser` cannot know — at its
//! snapshot the home always exists — so any attempt to infer it there would be
//! a check that always answers the same way in production.
//!
//! the opposite case — a **pre-existing** root-owned home with an existing user
//! — stays out: that directory is not ours to chown, and the installation stops
//! with an explicit precondition in
//! [`CreateOdooUser`](crate::steps::create_odoo_user).

use std::fs;
use std::os::unix::fs::PermissionsExt;

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// permissions of the freshly created root, owned by root while it waits.
const OPT_ROOT_MODE: u32 = 0o755;

/// permissions of the home once handed over. must match `HOME_MODE` in
/// [`CreateOdooUser`](crate::steps::create_odoo_user): the home must end up
/// identical whichever step handed it over.
const HANDED_OVER_MODE: u32 = 0o750;

/// creates `ctx.odoo_home` when missing, reversibly.
pub struct PrepareOptRoot {
    ops: Box<dyn SystemOps>,
    /// decided in `snapshot`, promoted to `CreatedByUs` by a `run` that
    /// actually created the directory.
    prestate: PreState,
}

impl PrepareOptRoot {
    /// constructor with injectable `SystemOps`, for the tests.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::default(),
        }
    }
}

impl Step for PrepareOptRoot {
    fn name(&self) -> &str {
        "prepare-opt-root"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        // already there means not ours, so the undo will be a no-op. otherwise
        // it stays `Untracked` until `run` really creates it.
        self.prestate = if ctx.odoo_home.exists() {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(
            dir = %ctx.odoo_home.display(),
            prestate = ?self.prestate,
            "snapshot prepare-opt-root"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let dir = &ctx.odoo_home;

        // pre-existing and not ours: neither owner nor permissions change.
        if self.prestate == PreState::Preexisting {
            info!(dir = %dir.display(), "run: directory già presente, nessuna azione");
            return Ok(());
        }

        if ctx.dry_run {
            info!(dir = %dir.display(), "run (dry-run): creerei la directory (owned root, 0755)");
            return Ok(());
        }

        // only the missing level, not `create_dir_all`: the rollback restores
        // exactly what we added, and not the parent too.
        fs::create_dir(dir).map_err(|e| StepError::io(dir, e))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(OPT_ROOT_MODE))
            .map_err(|e| StepError::io(dir, e))?;

        // ours from here, so the undo may remove it. set **before** any
        // handover: if the chown fails, the directory exists and is ours.
        self.prestate = PreState::CreatedByUs;

        // with the user already there the home is theirs at once: `owned root`
        // was only waiting for a later step that will now do nothing, seeing a
        // `Preexisting` user (A-V3-4).
        let user = &ctx.odoo_user;
        if self.ops.user_exists(user) {
            self.ops.chown_named(dir, user, user)?;
            self.ops.chmod(dir, HANDED_OVER_MODE)?;
            info!(
                dir = %dir.display(),
                user = %user,
                mode = format_args!("{HANDED_OVER_MODE:o}"),
                "run: directory creata e consegnata all'utente già esistente"
            );
            return Ok(());
        }

        info!(dir = %dir.display(), mode = format_args!("{OPT_ROOT_MODE:o}"), "run: directory creata (owned root, in attesa dell'utente)");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // invariant: the undo acts ONLY on what we created.
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (directory non creata da noi)");
            return Ok(());
        }

        let dir = &ctx.odoo_home;

        if ctx.dry_run {
            info!(dir = %dir.display(), "undo (dry-run): rimuoverei la directory");
            return Ok(());
        }

        // idempotent: already gone means nothing to do.
        if !dir.exists() {
            info!(dir = %dir.display(), "undo: directory già assente");
            return Ok(());
        }

        // remove ONLY if empty, never `rm -rf`: later steps' undos run first in
        // the reverse order, so by now it should be.
        let is_empty = match fs::read_dir(dir) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: impossibile leggere la directory, non rimuovo");
                return Ok(());
            }
        };

        if !is_empty {
            warn!(
                dir = %dir.display(),
                "undo: directory non vuota, non la rimuovo (best-effort, nessun rm -rf)"
            );
            return Ok(());
        }

        match fs::remove_dir(dir) {
            Ok(()) => info!(dir = %dir.display(), "undo: directory rimossa"),
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: rimozione fallita, proseguo (best-effort)")
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
