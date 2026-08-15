//! [`PrepareOptRoot`]: the first real step — creates the directories the
//! installation will live in.
//!
//! the simplest possible mutation, chosen as the reference model for the steps
//! that follow: a full `snapshot → run → undo` cycle with all three `PreState`s
//! handled.
//!
//! in Bash this `mkdir` hid inside `check_disk` (issue C4: a check that
//! mutates). here the check only measures, and creation is this reversible
//! step.
//!
//! # two levels, because two things are called "home"
//!
//! `/opt/odoo` is the **shared root**: every instance on the machine lives under
//! it, and it is created once. a **named** instance additionally gets its own
//! home — the install dir — which is the home of *its* system user and holds its
//! filestore, its cache and its config. for the unnamed instance the two are the
//! same directory, and everything below behaves exactly as it did before I0.
//!
//! both levels are snapshotted separately because ownership is per level: a
//! second instance finds `/opt/odoo` `Preexisting` (leave it alone) and its own
//! home `Untracked` (create it, and remove it on rollback). one `PreState` for
//! both would have made the second instance's rollback either destroy the shared
//! root or spare its own home.
//!
//! **ordering:** the `odoo` user usually does not exist yet, so the home is
//! created **owned root**; the `chown` happens in the user-creation step.
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
use std::path::Path;

use serde::{Deserialize, Serialize};
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

/// what this step created, level by level.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptRootSnapshot {
    /// the shared `/opt/odoo`.
    pub shared_root: PreState,
    /// this instance's own home, when it is a directory of its own.
    ///
    /// stays `Untracked` for the unnamed instance, whose home *is* the shared
    /// root: recording it twice would give the undo two claims on one directory.
    #[serde(default)]
    pub instance_home: PreState,
}

/// how this step's snapshot is read back from disk.
///
/// before I0 it was a bare `PreState` — one level was all there was. manifests
/// written by those versions are still on customers' machines, and a manifest
/// that cannot be rehydrated is an installation that can no longer be
/// uninstalled (the R7 rule, and the harm A-V3-1 caused). so both shapes are
/// accepted on the way in; only the current one is ever written out.
#[derive(Deserialize)]
#[serde(untagged)]
enum SnapshotRepr {
    /// pre-I0: a single `PreState` for `/opt/odoo`.
    Legacy(PreState),
    Current(OptRootSnapshot),
}

impl From<SnapshotRepr> for OptRootSnapshot {
    fn from(repr: SnapshotRepr) -> Self {
        match repr {
            // an old manifest describes the unnamed instance by construction,
            // so its single verdict is the shared root's and there is no second
            // level to speak of.
            SnapshotRepr::Legacy(shared_root) => OptRootSnapshot {
                shared_root,
                instance_home: PreState::Untracked,
            },
            SnapshotRepr::Current(snap) => snap,
        }
    }
}

/// creates the shared root and, for a named instance, its own home.
pub struct PrepareOptRoot {
    ops: Box<dyn SystemOps>,
    /// decided in `snapshot`, promoted to `CreatedByUs` by a `run` that
    /// actually created each directory.
    snap: OptRootSnapshot,
}

impl PrepareOptRoot {
    /// constructor with injectable `SystemOps`, for the tests.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: OptRootSnapshot::default(),
        }
    }

    /// the instance's own home, when it is a directory distinct from the shared
    /// root — that is, for a named instance.
    ///
    /// `None` for the unnamed instance says "there is no second level here",
    /// which is what keeps every branch below identical to its pre-I0 self.
    fn own_home(ctx: &Context) -> Option<std::path::PathBuf> {
        let home = ctx.user_home();
        (home != ctx.odoo_home).then_some(home)
    }

    /// creates one missing level, owned root.
    ///
    /// only the missing level, never `create_dir_all`: the rollback restores
    /// exactly what we added, and not the parent too.
    fn create_level(dir: &Path) -> Result<(), StepError> {
        fs::create_dir(dir).map_err(|e| StepError::io(dir, e))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(OPT_ROOT_MODE))
            .map_err(|e| StepError::io(dir, e))?;
        Ok(())
    }

    /// hands a directory we just created to an **already existing** user
    /// (A-V3-4).
    ///
    /// with the user already there, `owned root` was only waiting for a later
    /// step that will now do nothing, seeing a `Preexisting` user.
    fn hand_over_if_user_exists(&self, ctx: &Context, dir: &Path) -> Result<(), StepError> {
        let user = &ctx.odoo_user;
        if !self.ops.user_exists(user) {
            info!(
                dir = %dir.display(),
                mode = format_args!("{OPT_ROOT_MODE:o}"),
                "run: directory created (owned root, awaiting the user)"
            );
            return Ok(());
        }
        self.ops.chown_named(dir, user, user)?;
        self.ops.chmod(dir, HANDED_OVER_MODE)?;
        info!(
            dir = %dir.display(),
            user = %user,
            mode = format_args!("{HANDED_OVER_MODE:o}"),
            "run: directory created and handed to the already-existing user"
        );
        Ok(())
    }

    /// removes one level we created: only if it is still empty, never `rm -rf`.
    ///
    /// later steps' undos run first in the reverse order, so by now it should
    /// be. best-effort throughout: a leftover to remove by hand beats a
    /// directory destroyed on a guess.
    fn undo_level(prestate: &PreState, dir: &Path, dry_run: bool) {
        if *prestate != PreState::CreatedByUs {
            info!(dir = %dir.display(), prestate = ?prestate, "undo NO-OP (directory not created by us)");
            return;
        }
        if dry_run {
            info!(dir = %dir.display(), "undo (dry run): would remove the directory");
            return;
        }
        // idempotent: already gone means nothing to do.
        if !dir.exists() {
            info!(dir = %dir.display(), "undo: the directory is already gone");
            return;
        }
        let is_empty = match fs::read_dir(dir) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: cannot read the directory, not removing it");
                return;
            }
        };
        if !is_empty {
            warn!(
                dir = %dir.display(),
                "undo: the directory is not empty, leaving it (best-effort, never rm -rf)"
            );
            return;
        }
        match fs::remove_dir(dir) {
            Ok(()) => info!(dir = %dir.display(), "undo: directory removed"),
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: removal failed, proceeding (best-effort)")
            }
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
        self.snap.shared_root = if ctx.odoo_home.exists() {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.instance_home = match Self::own_home(ctx) {
            Some(home) if home.exists() => PreState::Preexisting,
            Some(_) => PreState::Untracked,
            None => PreState::Untracked,
        };
        info!(
            dir = %ctx.odoo_home.display(),
            prestate = ?self.snap.shared_root,
            instance_home = ?self.snap.instance_home,
            "snapshot: prepare-opt-root"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let own_home = Self::own_home(ctx);

        // --- the shared root ---------------------------------------------
        if self.snap.shared_root == PreState::Preexisting {
            info!(dir = %ctx.odoo_home.display(), "run: directory already present, nothing to do");
        } else if ctx.dry_run {
            info!(dir = %ctx.odoo_home.display(), "run (dry run): would create the directory (owned root, 0755)");
        } else {
            Self::create_level(&ctx.odoo_home)?;
            // ours from here, so the undo may remove it. set **before** any
            // handover: if the chown fails, the directory exists and is ours.
            self.snap.shared_root = PreState::CreatedByUs;
            match own_home {
                // a named instance keeps the shared root root-owned 0755: it is
                // shared, and handing it to one instance's user would give that
                // user the ground the other instances stand on.
                Some(_) => info!(
                    dir = %ctx.odoo_home.display(),
                    mode = format_args!("{OPT_ROOT_MODE:o}"),
                    "run: shared root created (owned root, shared by every instance)"
                ),
                // the unnamed instance's home *is* the shared root, so the
                // handover applies to it — exactly as before I0.
                None => self.hand_over_if_user_exists(ctx, &ctx.odoo_home)?,
            }
        }

        // --- this instance's own home, if it has one ----------------------
        let Some(home) = own_home else {
            return Ok(());
        };
        if self.snap.instance_home == PreState::Preexisting {
            info!(dir = %home.display(), "run: the instance home already exists, nothing to do");
            return Ok(());
        }
        if ctx.dry_run {
            info!(dir = %home.display(), "run (dry run): would create the instance home (owned root, 0755)");
            return Ok(());
        }
        Self::create_level(&home)?;
        self.snap.instance_home = PreState::CreatedByUs;
        self.hand_over_if_user_exists(ctx, &home)?;
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // deepest first: the instance home lives inside the shared root, which
        // only comes off once it is empty.
        if let Some(home) = Self::own_home(ctx) {
            Self::undo_level(&self.snap.instance_home, &home, ctx.dry_run);
        }
        // the shared root is not ours alone. with another instance still
        // installed it stays, whoever created it: every one of them lives
        // underneath (phase I2). the record stays in the manifest too, so the
        // last instance to go still knows the directory is its to remove.
        if ctx.shared_in_use {
            info!(
                dir = %ctx.odoo_home.display(),
                "undo NO-OP on the shared root: another instance still lives under it"
            );
            return Ok(());
        }
        Self::undo_level(&self.snap.shared_root, &ctx.odoo_home, ctx.dry_run);
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let repr: SnapshotRepr = decode_snapshot(self.name(), snapshot)?;
        self.snap = repr.into();
        Ok(())
    }
}
