//! [`CreateOdooUser`]: creates the `odoo` system user and gives it the home.
//!
//! follows [`crate::steps::prepare_opt_root`]'s model on a richer resource:
//! user, group and home ownership.
//!
//! # coordinating with `PrepareOptRoot`
//!
//! that step creates `/opt/odoo`; this one makes it `odoo:odoo`. the rollback's
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
            "l'utente di sistema '{user}' esiste già, ma la sua home {home} appartiene a root \
             e non è stata creata da questa installazione.\n\
             \n\
             Così com'è, l'utente '{user}' non può scrivere nella propria home e \
             l'installazione fallirebbe più avanti con un errore poco chiaro.\n\
             \n\
             Sistemala a mano scegliendo tu cosa è giusto per questa macchina:\n\
               sudo chown -R {user}:{user} {home}     (se quella directory è destinata a Odoo)\n\
             oppure rimuovila, se è un residuo, e rilancia l'installer.",
            user = ctx.odoo_user,
            home = ctx.odoo_home.display()
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
        self.snap.home_original_owner = if self.ops.path_exists(&ctx.odoo_home) {
            self.ops.owner_of(&ctx.odoo_home).ok()
        } else {
            None
        };

        info!(
            user = %user,
            prestate = ?self.snap.user_prestate,
            home_owner = ?self.snap.home_original_owner,
            "snapshot create-odoo-user"
        );

        self.refuse_unusable_home(ctx)?;
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let user = &ctx.odoo_user;
        let home = &ctx.odoo_home;

        // pre-existing user: not ours. no `useradd`, and deliberately no
        // aggressive chown on a situation that is not ours either.
        if self.snap.user_prestate == PreState::Preexisting {
            info!(user = %user, "run: utente già presente, skip creazione (nessun chown aggressivo)");
            return Ok(());
        }

        if ctx.dry_run {
            info!(
                user = %user,
                home = %home.display(),
                "run (dry-run): creerei l'utente (useradd --system --create-home --user-group --shell /bin/false) e chown {user}:{user} 0750"
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
        // `useradd` does not re-chown a pre-existing home, so we do.
        self.ops.chown_named(home, user, user)?;
        self.ops.chmod(home, HOME_MODE)?;

        self.snap.user_prestate = PreState::CreatedByUs;
        info!(user = %user, home = %home.display(), "run: utente creato, home owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // acts only on users we created. never touch a pre-existing one.
        if self.snap.user_prestate != PreState::CreatedByUs {
            info!(prestate = ?self.snap.user_prestate, "undo NO-OP (utente non creato da noi)");
            return Ok(());
        }

        let user = &ctx.odoo_user;

        if ctx.dry_run {
            info!(user = %user, "undo (dry-run): userdel (senza -r) + groupdel + ripristino owner home");
            return Ok(());
        }

        // `userdel` WITHOUT `-r`: the home is `PrepareOptRoot`'s to remove.
        if let Err(e) = self.ops.delete_user(user) {
            warn!(user = %user, error = %e, "undo: userdel fallito, proseguo (best-effort)");
        }

        // the dedicated group, if it outlived the user. on some families
        // `userdel` already took it, and finding nothing is the **wanted**
        // outcome rather than a failure (A-MD-3).
        if let Err(e) = self.ops.delete_group(user) {
            if group_already_gone(&e) {
                info!(
                    group = %user,
                    "undo: il gruppo non esiste più — l'ha già rimosso `userdel` insieme \
                     all'utente. È il risultato voluto"
                );
            } else {
                warn!(group = %user, error = %e, "undo: groupdel fallito, proseguo (best-effort)");
            }
        }

        // restore the home's original owner if we changed it. when the home is
        // `PrepareOptRoot`'s it will be removed anyway, so this is a harmless
        // best-effort.
        if let Some(original) = self.snap.home_original_owner {
            if self.ops.path_exists(&ctx.odoo_home) {
                if let Err(e) = self.ops.chown_numeric(&ctx.odoo_home, original) {
                    warn!(error = %e, "undo: ripristino owner home fallito, proseguo (best-effort)");
                } else {
                    info!(owner = ?original, "undo: owner originale della home ripristinato");
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
