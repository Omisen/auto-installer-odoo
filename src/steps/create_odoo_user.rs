//! [`CreateOdooUser`]: crea l'utente di sistema `odoo` e ne rende owner la home.
//!
//! Segue il modello di [`crate::steps::prepare_opt_root`], ma su una risorsa più
//! ricca (utente + gruppo + ownership della home).
//!
//! # Coordinamento con `PrepareOptRoot` (il punto delicato)
//!
//! `/opt/odoo` è creata da [`PrepareOptRoot`](crate::steps::prepare_opt_root),
//! non da questo step. Qui la directory diventa `odoo:odoo`. Regola di
//! ownership del rollback: **ogni step possiede la rimozione di ciò che ha
//! creato**. Perciò:
//!
//! - `undo` esegue `userdel` **senza `-r`**: NON rimuove la home. La home la
//!   rimuove `PrepareOptRoot.undo`, che gira *dopo* (ordine inverso).
//! - se la home era `Preexisting` (non nostra) e il nostro `chown` ne ha
//!   cambiato l'owner, `undo` ripristina l'owner originale salvato in
//!   `snapshot` — così non resta di proprietà di un utente che stiamo
//!   cancellando.
//!
//! Regola invariante di `CLAUDE.md`: **mai `userdel -r` su un utente
//! `Preexisting`** (cancellerebbe una home non nostra). Qui l'undo agisce solo
//! su utenti `CreatedByUs`, e comunque senza `-r`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{OwnerId, RealSystemOps, SystemOps, UserSpec};

/// Permessi della home (owner rwx, group r-x, other ---).
const HOME_MODE: u32 = 0o750;
/// Nessuna shell interattiva (principio del privilegio minimo).
const LOGIN_SHELL: &str = "/bin/false";

/// Snapshot serializzabile dello step, sufficiente a ricostruire l'undo.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CreateUserSnapshot {
    /// `Preexisting` se l'utente c'era già; `CreatedByUs` dopo un run che l'ha
    /// creato; `Untracked` altrimenti.
    pub user_prestate: PreState,
    /// Owner della home **prima** del nostro `chown` (per ripristinarlo in undo
    /// se la home era `Preexisting`). `None` se la home non esisteva.
    pub home_original_owner: Option<OwnerId>,
}

/// Crea l'utente di sistema `odoo` (reversibile) e ne rende owner la home.
pub struct CreateOdooUser {
    ops: Box<dyn SystemOps>,
    snap: CreateUserSnapshot,
}

impl CreateOdooUser {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    /// Costruttore con `SystemOps` iniettabile (usato dai test con un mock).
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: CreateUserSnapshot::default(),
        }
    }
}

impl Default for CreateOdooUser {
    fn default() -> Self {
        Self::new()
    }
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

        // Owner della home PRIMA del nostro eventuale chown: serve a un undo
        // corretto se la home era preesistente (non nostra).
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
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let user = &ctx.odoo_user;
        let home = &ctx.odoo_home;

        // Utente preesistente: non è nostro. Nessun useradd e — scelta
        // conservativa — nessun chown aggressivo su una situazione non nostra.
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
        // useradd non ri-chowna una home preesistente: lo facciamo esplicitamente.
        self.ops.chown_named(home, user, user)?;
        self.ops.chmod(home, HOME_MODE)?;

        self.snap.user_prestate = PreState::CreatedByUs;
        info!(user = %user, home = %home.display(), "run: utente creato, home owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // Agisce solo su utenti creati da noi. Mai toccare un Preexisting.
        if self.snap.user_prestate != PreState::CreatedByUs {
            info!(prestate = ?self.snap.user_prestate, "undo NO-OP (utente non creato da noi)");
            return Ok(());
        }

        let user = &ctx.odoo_user;

        if ctx.dry_run {
            info!(user = %user, "undo (dry-run): userdel (senza -r) + groupdel + ripristino owner home");
            return Ok(());
        }

        // 1) userdel SENZA -r: la home /opt/odoo la rimuove PrepareOptRoot.undo.
        if let Err(e) = self.ops.delete_user(user) {
            warn!(user = %user, error = %e, "undo: userdel fallito, proseguo (best-effort)");
        }

        // 2) Gruppo dedicato creato da --user-group, se resta orfano.
        if let Err(e) = self.ops.delete_group(user) {
            warn!(group = %user, error = %e, "undo: groupdel fallito/orfano, proseguo (best-effort)");
        }

        // 3) Ripristina l'owner originale della home se l'avevamo cambiato
        //    (rilevante quando la home era Preexisting). Se invece è di
        //    PrepareOptRoot, verrà rimossa comunque: qui è best-effort innocuo.
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
