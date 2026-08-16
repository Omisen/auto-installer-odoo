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
//!
//! # the shared root has to be walkable by everybody under it (`A-V6-9`)
//!
//! the unnamed instance's home *is* `/opt/odoo`, so it is handed over as
//! `odoo:odoo 0750` — right while it is one instance's private home, wrong the
//! moment a second one moves in. adding `--instance cliente-x` to that machine
//! creates `odoo-cliente-x`, a user that is neither the owner nor in the group:
//! with `0750` it cannot **traverse** `/opt/odoo`, so it never reaches its own
//! home. that is the migration path of every existing customer, and in the field
//! it failed at `setup-cache-dir` with `mkdir: cannot create directory
//! '/opt/odoo': Permission denied` — a directory that *exists*, which sends
//! whoever reads it looking for the wrong problem.
//!
//! so a named instance that finds the shared root **not traversable** widens it
//! by exactly one bit (`o+x`: walk through, still not list), and records the
//! mode it found. the widening is an artifact like any other — R11's rule on the
//! nginx default site, applied to a mode instead of a symlink: we touch what the
//! customer owns *only* by writing down what it was.
//!
//! `o+x` and not `o+rx` because traversal is all that is needed: the contents
//! stay unlistable to third parties, and each instance's own home keeps its
//! `0750`.
//!
//! ## the bit can outlive the instance that set it, and that is **accepted**
//!
//! the annotation lives in the manifest of whoever widened. with **two** named
//! instances, if the widener leaves first the bit has to stay — the other one
//! still walks through it — and when that other one leaves, its own manifest
//! has no mode to put back, because it found the root already traversable.
//!
//! **the record is not lost, though**, and the audit used to say otherwise: an
//! undo that declines to restore keeps its step in the manifest, so the widener
//! departs as a *tombstone* still carrying `shared_root_mode_before`. It is
//! `rollback --all` that comes back for tombstones, and there the mode does go
//! back. What remains is the narrower case: named instances removed one at a
//! time on a machine that keeps the historical one.
//!
//! closing even that would mean a **refcount on a permission** — machinery with
//! its own ambiguities (two tombstones recording different modes: which wins?)
//! for a residue that is one *traversal* bit on a directory whose contents stay
//! `0750`. Nothing is exposed by it: you cannot list, and you cannot read what
//! you had no right to. So it is accepted.
//!
//! accepted, **not silent**. Until A-V6-11-bis the skipped restoration said
//! nothing at all, which is the part that was actually wrong: the customer saw
//! `0751` and had no way to tell our artifact from their own configuration. See
//! [`PrepareOptRoot::announce_mode_held`].

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

/// the one bit a named instance needs on the shared root: **traverse**, not
/// list (`A-V6-9`).
const TRAVERSE_BY_OTHERS: u32 = 0o001;

/// what to say when the traversal bit is **kept on purpose**, or `None` when
/// there is nothing to say.
///
/// pure and returning the text, for the reason A-R9-1 taught: when a check's
/// value is in its **wording**, asserting its outcome asserts nothing — and a
/// message only reachable by capturing logs is a message no test looks at. same
/// shape as [`crate::checks::untested_release_warning`].
///
/// `None` in the two cases where the sentence would be false: we widened
/// nothing, so there is no bit of ours to hold; or the only neighbours left are
/// unnamed, and the unnamed instance **owns** the root — it never needed the
/// bit, so it is not what is keeping it (`A-V6-9`).
pub fn held_mode_notice(before: Option<u32>, others: &[String]) -> Option<String> {
    let before = before?;
    let traversers: Vec<&str> = others
        .iter()
        .map(String::as_str)
        .filter(|name| *name != crate::instance::UNNAMED_ID)
        .collect();
    if traversers.is_empty() {
        return None;
    }
    Some(format!(
        "the shared root keeps its traversal bit ({:o} instead of {:o}): the named instances {} \
         still have to walk through it to reach their own homes. the mode to put back stays \
         recorded in this manifest and is restored when the last of them is removed.",
        before | TRAVERSE_BY_OTHERS,
        before,
        traversers.join(", ")
    ))
}

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
    /// the shared root's mode **before** we widened it, and `None` when we did
    /// not touch it (`A-V6-9`).
    ///
    /// `None` covers three different situations that need no undo and must not
    /// be told apart here: we created the root ourselves, it was already
    /// traversable, or this is the unnamed instance. what they have in common is
    /// the only thing the undo asks — there is no foreign mode of ours to put
    /// back.
    #[serde(default)]
    pub shared_root_mode_before: Option<u32>,
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
                shared_root_mode_before: None,
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

    /// decides — **without mutating**, this runs in `snapshot` — whether the
    /// shared root has to be widened for this instance's user to reach its home
    /// (`A-V6-9`), and returns the mode found so the undo can put it back.
    ///
    /// # errors
    ///
    /// [`StepError::Precondition`] when the mode cannot be read. an unreadable
    /// mode is "I do not know", never "it is fine": widening without having read
    /// what was there would be a mutation with no undo, and *not* widening would
    /// send the installation into the misleading `mkdir` failure this exists to
    /// prevent. so it stops here, before anything is touched, with a message
    /// that names the permission.
    fn traversal_to_widen(&self, ctx: &Context) -> Result<Option<u32>, StepError> {
        // the unnamed instance is the root's owner: it walks in as itself, and
        // widening would open one instance's private home to third parties.
        let Some(home) = Self::own_home(ctx) else {
            return Ok(None);
        };
        // a root we are about to create is born 0755 — traversable by design,
        // because it is shared by design.
        if self.snap.shared_root != PreState::Preexisting {
            return Ok(None);
        }
        let dir = &ctx.odoo_home;
        let mode = self.ops.mode_of(dir).map_err(|e| {
            StepError::Precondition(format!(
                "cannot read the permissions of {}: {e}. user '{}' has to traverse it to reach \
                 {}, and without knowing what the mode is now there is no way to widen it and \
                 put it back afterwards",
                dir.display(),
                ctx.odoo_user,
                home.display()
            ))
        })?;
        if mode & TRAVERSE_BY_OTHERS != 0 {
            info!(
                dir = %dir.display(),
                mode = format_args!("{mode:o}"),
                "snapshot: the shared root is already traversable, nothing to widen"
            );
            return Ok(None);
        }
        Ok(Some(mode))
    }

    /// applies the widening decided in `snapshot`.
    ///
    /// `warn` and not `info`: this is the one place the step touches something
    /// somebody else owns, and the log is where a customer's post-mortem looks.
    fn widen_shared_root(&self, ctx: &Context) -> Result<(), StepError> {
        let Some(before) = self.snap.shared_root_mode_before else {
            return Ok(());
        };
        let widened = before | TRAVERSE_BY_OTHERS;
        let dir = &ctx.odoo_home;
        if ctx.dry_run {
            info!(
                dir = %dir.display(),
                from = format_args!("{before:o}"),
                to = format_args!("{widened:o}"),
                "run (dry run): would widen the shared root so this instance can traverse it"
            );
            return Ok(());
        }
        self.ops.chmod(dir, widened)?;
        warn!(
            dir = %dir.display(),
            from = format_args!("{before:o}"),
            to = format_args!("{widened:o}"),
            user = %ctx.odoo_user,
            "run: the shared root was not traversable by this instance's user; widened by o+x \
             (walk through, still not list). the rollback puts the mode back"
        );
        Ok(())
    }

    /// puts the shared root's mode back — only what we changed, and only if it
    /// is still what we left.
    ///
    /// says that the widened bit is being **kept on purpose**, and why.
    ///
    /// the other branch of [`Self::restore_shared_root_mode`], and until
    /// A-V6-11-bis it did not exist: with another **named** instance still
    /// needing to walk in, the restoration was skipped in complete silence. the
    /// customer removed an instance, found `/opt/odoo` at `0751`, and nothing
    /// anywhere said whether that was ours, theirs, or a mistake — while the
    /// mode we would put back was sitting in this very snapshot.
    ///
    /// the residue itself is accepted, not fixed (see the module docs): what is
    /// not acceptable is an artifact of ours that no message accounts for. this
    /// is the same reasoning as A-MD-2 — the *verdict* was wrong there, the
    /// *silence* is wrong here.
    fn announce_mode_held(&self, ctx: &Context) {
        if let Some(notice) =
            held_mode_notice(self.snap.shared_root_mode_before, &ctx.other_instances)
        {
            info!(dir = %ctx.odoo_home.display(), "{notice}");
        }
    }

    /// never fails the rollback: like every undo it is best-effort, and a mode
    /// left wide is a residue, not a loss.
    fn restore_shared_root_mode(&self, ctx: &Context) {
        let Some(before) = self.snap.shared_root_mode_before else {
            return;
        };
        let dir = &ctx.odoo_home;
        if ctx.dry_run {
            info!(dir = %dir.display(), mode = format_args!("{before:o}"), "undo (dry run): would put the shared root's mode back");
            return;
        }
        if !dir.exists() {
            info!(dir = %dir.display(), "undo: the shared root is already gone, no mode to put back");
            return;
        }
        let widened = before | TRAVERSE_BY_OTHERS;
        match self.ops.mode_of(dir) {
            // the .bashrc rule, on a mode: we put back what we changed, or we
            // leave it alone — never a mode somebody else may have chosen since.
            Ok(now) if now != widened => warn!(
                dir = %dir.display(),
                now = format_args!("{now:o}"),
                left = format_args!("{widened:o}"),
                "undo: the shared root's mode is not the one we left, leaving it alone"
            ),
            Ok(_) => match self.ops.chmod(dir, before) {
                Ok(()) => info!(
                    dir = %dir.display(),
                    mode = format_args!("{before:o}"),
                    "undo: the shared root's mode is back to what we found"
                ),
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "undo: could not put the mode back (best-effort)")
                }
            },
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: cannot read the mode, leaving it alone")
            }
        }
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
        // reads only, and decides: the mutation is `run`'s (C4).
        self.snap.shared_root_mode_before = self.traversal_to_widen(ctx)?;
        info!(
            dir = %ctx.odoo_home.display(),
            prestate = ?self.snap.shared_root,
            instance_home = ?self.snap.instance_home,
            widen_from = ?self.snap.shared_root_mode_before.map(|m| format!("{m:o}")),
            "snapshot: prepare-opt-root"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let own_home = Self::own_home(ctx);

        // --- the shared root ---------------------------------------------
        if self.snap.shared_root == PreState::Preexisting {
            info!(dir = %ctx.odoo_home.display(), "run: directory already present, nothing to do");
            // …except, for a named instance, making it walkable (A-V6-9). the
            // directory stays somebody else's; one bit of its mode becomes ours
            // to put back.
            self.widen_shared_root(ctx)?;
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
        // the widened bit comes back as soon as nobody else has to walk in —
        // which is NOT the same question as "is anybody else installed". a
        // machine left with only the historical instance gets its `0750` back:
        // that instance owns the root and never needed the bit.
        if ctx.shared_root_traversed_by_others() {
            self.announce_mode_held(ctx);
        } else {
            self.restore_shared_root_mode(ctx);
        }
        if ctx.shared_in_use() {
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
