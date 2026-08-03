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
use crate::system_ops::{OwnerId, SystemOps, UserSpec};

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
    /// Costruttore con `SystemOps` iniettabile (usato dai test con un mock).
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: CreateUserSnapshot::default(),
        }
    }
}

impl CreateOdooUser {
    /// Precondizione: se l'utente esiste già, la sua home dev'essere usabile da
    /// lui (A-V3-4).
    ///
    /// # Il caso che intercetta
    ///
    /// Utente `odoo` preesistente **e** `/opt/odoo` preesistente di proprietà di
    /// root. Da qui l'installazione è impossibile: questo step non fa il `chown`
    /// (scelta deliberata — la directory non è nostra), e tre step più avanti
    /// `SetupCacheDir` esegue `sudo -u odoo mkdir -p /opt/odoo/.cache` su una
    /// directory di root. Il risultato era un *Permission denied* su un `mkdir`,
    /// senza alcun indizio sulla causa vera, che sta due step prima e in una
    /// combinazione di condizioni che l'utente non ha modo di indovinare.
    ///
    /// Fermarsi **qui** e con un messaggio esplicito è meglio: è una
    /// precondizione, come l'hard-stop sull'init del database. Non è un `undo`
    /// da scrivere, è una mutazione da non iniziare.
    ///
    /// # Cosa NON copre, dichiarato
    ///
    /// Il controllo guarda se la home è di **root** (`uid 0`), non se appartiene
    /// a un terzo utente qualsiasi: quello richiederebbe di risolvere l'uid del
    /// nostro utente, e il caso realistico — una directory di sistema creata da
    /// root — è questo. Il messaggio dice cosa ha trovato, non di più.
    ///
    /// # Perché il caso "home creata da noi" non arriva qui
    ///
    /// Se `/opt/odoo` la crea [`PrepareOptRoot`](crate::steps::prepare_opt_root)
    /// e l'utente esiste già, è quello step a consegnargliela subito. Quando
    /// arriviamo qui la home è già dell'utente e la precondizione passa. La
    /// distinzione fra "creata da noi" e "preesistente" vive lì perché è lì che
    /// l'informazione esiste.
    fn refuse_unusable_home(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.user_prestate != PreState::Preexisting {
            return Ok(());
        }
        let Some(owner) = self.snap.home_original_owner else {
            // Home assente: siamo in dry-run (in un'esecuzione reale
            // `PrepareOptRoot` l'ha appena creata). Niente da verificare.
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

/// L'esito di `groupdel` che l'audit chiama A-MD-3: **il gruppo non c'era già**.
///
/// # Perché va distinto da un fallimento
///
/// Su Fedora `userdel` rimuove anche il gruppo primario dell'utente; il
/// `groupdel` che segue esce quindi 6 («specified group doesn't exist»). Su
/// Debian/Ubuntu il gruppo sopravvive all'utente e il `groupdel` serve davvero:
/// è una divergenza di comportamento fra le famiglie che nessuno aveva previsto.
///
/// L'undo è corretto in entrambi i casi — il gruppo *non c'è*, che è il
/// risultato voluto — ma finché lo comunicava come `WARN` chi leggeva un
/// rollback riuscito trovava un avviso e si chiedeva se qualcosa fosse rimasto
/// indietro. È la categoria di A-V3-10: cosmetico, e proprio per questo
/// insidioso, perché compare **a ogni rollback** e insegna a ignorare i warning.
///
/// # Perché l'exit code e non il messaggio
///
/// Perché `groupdel` scrive «group 'odoo' does not exist» **nella lingua del
/// sistema**, e un controllo sullo stderr fallirebbe su una macchina localizzata
/// — la stessa trappola di `apt-cache policy` in R6, dove è servito `LC_ALL=C`.
/// Il codice 6 è documentato da shadow-utils e non si traduce.
///
/// Pura: si verifica su un errore costruito a mano, senza avere un gruppo da
/// rimuovere né i privilegi per farlo.
pub fn group_already_gone(err: &StepError) -> bool {
    /// `groupdel`: «specified group doesn't exist» (shadow-utils).
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

        self.refuse_unusable_home(ctx)?;
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
        //
        // Su alcune famiglie `userdel` porta via anche il gruppo primario, e
        // questo `groupdel` trova il vuoto: è l'esito **voluto**, non un
        // fallimento, e va detto come tale (A-MD-3).
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
