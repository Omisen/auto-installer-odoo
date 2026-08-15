//! [`CreateOdooUser`]: creates the `odoo` system user and gives it the home.
//!
//! follows [`crate::steps::prepare_opt_root`]'s model on a richer resource:
//! user, group and home ownership.
//!
//! # coordinating with `PrepareOptRoot`
//!
//! that step creates the home — `/opt/odoo` for the unnamed instance, the
//! install dir for a named one — and this one makes it `<user>:<user>`. which
//! directory that is comes from [`Context::user_home`](crate::context::Context::user_home),
//! never from `odoo_home` directly: for a named instance the shared root is not
//! this user's home and must not be chowned to it. the rollback's
//! ownership rule is **every step owns the removal of what it created**, so:
//!
//! - the undo runs `userdel` **without `-r`** and does NOT remove the home,
//!   which `PrepareOptRoot`'s undo removes later in the reverse order;
//! - if the home was `Preexisting` and our `chown` changed its owner, the undo
//!   restores the original, so it is not left owned by a user we are deleting.
//!
//! invariant from `CLAUDE.md`: **never `userdel -r` on a `Preexisting` user**.
//! here the undo acts only on `CreatedByUs` ones, and without `-r` anyway.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{OwnerId, SystemOps, UserSpec};

/// home permissions.
const HOME_MODE: u32 = 0o750;
/// no interactive shell: least privilege.
const LOGIN_SHELL: &str = "/bin/false";

/// the step's serialisable snapshot, enough to rebuild the undo.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CreateUserSnapshot {
    /// whether the user was already there, or we created it.
    pub user_prestate: PreState,
    /// the home's owner **before** our `chown`, so the undo can restore it.
    /// `None` when the home did not exist.
    pub home_original_owner: Option<OwnerId>,
    /// the home's mode **before** our `chmod` (`A-V6-12`), same reason and same
    /// `None`.
    ///
    /// the owner alone was half the answer: `run` sets `0750` on a home that may
    /// have been somebody else's with a mode of their choosing, and an undo that
    /// hands the directory back with different permissions has not put it back.
    /// found by the model once it started remembering modes at all — the same
    /// way R13 found `A-V3-7`: a mock that answers like the real command.
    #[serde(default)]
    pub home_original_mode: Option<u32>,
}

/// creates the `odoo` system user, reversibly, and gives it the home.
pub struct CreateOdooUser {
    ops: Box<dyn SystemOps>,
    snap: CreateUserSnapshot,
}

impl CreateOdooUser {
    /// constructor with injectable `SystemOps`, for the tests.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: CreateUserSnapshot::default(),
        }
    }
}

impl CreateOdooUser {
    /// precondition: an already-existing user must be able to use its home
    /// (A-V3-4).
    ///
    /// a pre-existing `odoo` user **and** a pre-existing root-owned `/opt/odoo`
    /// makes the installation impossible: this step deliberately does not chown
    /// a directory that is not ours, and three steps later `SetupCacheDir` hits
    /// *Permission denied* on a `mkdir`, with no clue about a cause two steps
    /// back.
    ///
    /// stopping **here** with an explicit message is better: a precondition,
    /// like the database init's hard stop. not an undo to write but a mutation
    /// not to begin.
    ///
    /// declared limitation: it checks whether the home belongs to **root**, not
    /// to some third user — that would mean resolving our user's uid, and the
    /// realistic case is a system directory created by root.
    ///
    /// the "home created by us" case never arrives here: `PrepareOptRoot` hands
    /// it over at once, because that is where the information lives.
    ///
    /// # errors
    ///
    /// [`StepError::Precondition`] naming the home, its owner and the two ways
    /// out.
    fn refuse_unusable_home(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.user_prestate != PreState::Preexisting {
            return Ok(());
        }
        let Some(owner) = self.snap.home_original_owner else {
            // no home means a dry run: in a real one `PrepareOptRoot` has just
            // created it.
            return Ok(());
        };
        if owner.uid != 0 {
            return Ok(());
        }

        Err(StepError::Precondition(format!(
            "system user '{user}' already exists, but its home {home} belongs to root and \
             was not created by this installation.\n\
             \n\
             as it stands, '{user}' cannot write in its own home and the installation would \
             fail later with an unclear error.\n\
             \n\
             fix it by hand, choosing what is right for this machine:\n\
               sudo chown -R {user}:{user} {home}     (if that directory is meant for Odoo)\n\
             or remove it, if it is a leftover, and run the installer again.",
            user = ctx.odoo_user,
            home = ctx.user_home().display()
        )))
    }
}

/// the `groupdel` outcome A-MD-3 names: **the group was already gone**.
///
/// on Fedora `userdel` takes the primary group with it, so the following
/// `groupdel` exits 6; on Debian the group outlives the user and the call is
/// needed. an unforeseen behavioural divergence between the families.
///
/// the undo is correct either way — the group *is not there*, which is the
/// wanted result — but reporting it as a `WARN` made every successful rollback
/// look suspicious. A-V3-10's category: cosmetic, and insidious precisely
/// because it appears **every time** and teaches people to ignore warnings.
///
/// the exit code and not the message, because `groupdel` writes in the
/// **system's language** and a check on stderr would fail on a localised
/// machine — `apt-cache policy`'s trap in R6. code 6 is documented by
/// shadow-utils and does not translate.
///
/// pure: checkable on a hand-built error, without a group to remove or the
/// privileges to remove it.
pub fn group_already_gone(err: &StepError) -> bool {
    /// `groupdel`: "specified group doesn't exist".
    const GROUP_NOT_FOUND: &str = "6";
    matches!(err, StepError::CommandFailed { status, .. } if status == GROUP_NOT_FOUND)
}

impl Step for CreateOdooUser {
    fn name(&self) -> &str {
        "create-odoo-user"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let user = &ctx.odoo_user;

        self.snap.user_prestate = if self.ops.user_exists(user) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };

        // the home's owner BEFORE any chown of ours, for a correct undo when it
        // was pre-existing.
        let home = ctx.user_home();
        let home_there = self.ops.path_exists(&home);
        self.snap.home_original_owner = if home_there {
            self.ops.owner_of(&home).ok()
        } else {
            None
        };
        self.snap.home_original_mode = if home_there {
            self.ops.mode_of(&home).ok()
        } else {
            None
        };

        info!(
            user = %user,
            prestate = ?self.snap.user_prestate,
            home_owner = ?self.snap.home_original_owner,
            home_mode = ?self.snap.home_original_mode.map(|m| format!("{m:o}")),
            "snapshot: create-odoo-user"
        );

        self.refuse_unusable_home(ctx)?;
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let user = &ctx.odoo_user;
        let home = ctx.user_home();

        // pre-existing user: not ours. no `useradd`, and deliberately no
        // aggressive chown on a situation that is not ours either.
        if self.snap.user_prestate == PreState::Preexisting {
            info!(user = %user, "run: user already present, skipping creation (no aggressive chown)");
            return Ok(());
        }

        if ctx.dry_run {
            info!(
                user = %user,
                home = %home.display(),
                "run (dry run): would create the user (useradd --system --create-home --user-group --shell /bin/false) and chown {user}:{user} 0750"
            );
            return Ok(());
        }

        let spec = UserSpec {
            name: user.clone(),
            home: home.clone(),
            system: true,
            create_home: true,
            user_group: true,
            shell: LOGIN_SHELL.to_string(),
        };
        self.ops.create_user(&spec)?;
        // ours from here (A-V3-24): a `chown` or `chmod` that fails below must
        // not leave a system user nobody will ever remove.
        self.snap.user_prestate = PreState::CreatedByUs;
        // `useradd` does not re-chown a pre-existing home, so we do.
        self.ops.chown_named(&home, user, user)?;
        self.ops.chmod(&home, HOME_MODE)?;
        info!(user = %user, home = %home.display(), "run: user created, home owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // acts only on users we created. never touch a pre-existing one.
        if self.snap.user_prestate != PreState::CreatedByUs {
            info!(prestate = ?self.snap.user_prestate, "undo NO-OP (user not created by us)");
            return Ok(());
        }

        let user = &ctx.odoo_user;

        if ctx.dry_run {
            info!(user = %user, "undo (dry run): userdel (without -r) + groupdel + restore of the home owner");
            return Ok(());
        }

        // `userdel` WITHOUT `-r`: the home is `PrepareOptRoot`'s to remove.
        if let Err(e) = self.ops.delete_user(user) {
            warn!(user = %user, error = %e, "undo: userdel failed, proceeding (best-effort)");
        }

        // the dedicated group, if it outlived the user. on some families
        // `userdel` already took it, and finding nothing is the **wanted**
        // outcome rather than a failure (A-MD-3).
        if let Err(e) = self.ops.delete_group(user) {
            if group_already_gone(&e) {
                info!(
                    group = %user,
                    "undo: the group is gone — `userdel` removed it with the user. that is \
                     the intended outcome"
                );
            } else {
                warn!(group = %user, error = %e, "undo: groupdel failed, proceeding (best-effort)");
            }
        }

        // restore the home's original owner if we changed it. when the home is
        // `PrepareOptRoot`'s it will be removed anyway, so this is a harmless
        // best-effort.
        if self.snap.home_original_owner.is_some() || self.snap.home_original_mode.is_some() {
            let home = ctx.user_home();
            if self.ops.path_exists(&home) {
                if let Some(original) = self.snap.home_original_owner {
                    if let Err(e) = self.ops.chown_numeric(&home, original) {
                        warn!(error = %e, "undo: restoring the home owner failed, proceeding (best-effort)");
                    } else {
                        info!(owner = ?original, "undo: the home's original owner was restored");
                    }
                }
                // owner and mode are one restoration, not two: handing a
                // directory back to its owner with permissions we chose is
                // still not handing it back.
                if let Some(original) = self.snap.home_original_mode {
                    if let Err(e) = self.ops.chmod(&home, original) {
                        warn!(error = %e, "undo: restoring the home mode failed, proceeding (best-effort)");
                    } else {
                        info!(
                            mode = format_args!("{original:o}"),
                            "undo: the home's original mode was restored"
                        );
                    }
                }
            }
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
