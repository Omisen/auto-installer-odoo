//! [`PrepareOptRoot`]: primo step reale — crea `/opt/odoo` se manca.
//!
//! È la mutazione più semplice possibile (una directory) ed è scelta apposta
//! come modello di riferimento per gli step successivi: implementa il ciclo
//! completo `snapshot → run → undo` con i tre `PreState` gestiti.
//!
//! Nel Bash questo `mkdir` era nascosto dentro `check_disk` (criticità C4: un
//! check che muta). Qui il check misura soltanto ([`crate::checks::check_disk`])
//! e la creazione è questo step reversibile.
//!
//! **Dipendenza d'ordine (importante):** di norma a questo punto l'utente `odoo`
//! non esiste ancora (viene creato in Fase 3). Perciò la directory è creata
//! **owned root** con permessi `0755`; il `chown odoo:odoo` avverrà nello step di
//! creazione utente, che è l'ordine corretto.
//!
//! # Se l'utente esiste già, la consegna avviene qui (A-V3-4)
//!
//! `owned root` non è la condizione *giusta* della home: è una condizione
//! **d'attesa**, che ha senso solo finché l'utente non c'è. Quando l'utente
//! esiste già — un pacchetto di distro, un residuo di un setup manuale, una
//! convenzione aziendale — non c'è niente da attendere, e lasciarla root-owned
//! rompeva l'installazione tre step più avanti: `CreateOdooUser` vede l'utente
//! `Preexisting` e ritorna subito senza fare il `chown` (scelta deliberata: non
//! si tocca ciò che non è nostro), e `SetupCacheDir` esegue
//! `sudo -u odoo mkdir -p /opt/odoo/.cache` su una directory di root →
//! *Permission denied*, con un errore che non dice niente sulla causa.
//!
//! La consegna sta **qui** e non là perché qui c'è l'informazione che serve:
//! questo step sa di aver creato lui la directory. `CreateOdooUser` non può
//! saperlo — al suo snapshot la home esiste sempre, l'abbiamo appena creata noi,
//! quindi nessun dato in suo possesso distingue «l'ho creata io un istante fa»
//! da «c'era già». Ogni tentativo di dedurlo da lì sarebbe un controllo che in
//! produzione risponde sempre allo stesso modo.
//!
//! Il caso opposto — home **preesistente** e root-owned, con l'utente già
//! esistente — resta fuori: quella directory non è nostra e non la chowniamo.
//! Lì l'installazione si ferma con una precondizione esplicita in
//! [`CreateOdooUser`](crate::steps::create_odoo_user).

use std::fs;
use std::os::unix::fs::PermissionsExt;

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// Permessi della directory radice appena creata (owned root in attesa
/// dell'utente).
const OPT_ROOT_MODE: u32 = 0o755;

/// Permessi della home una volta consegnata al suo utente. Deve coincidere con
/// `HOME_MODE` di [`CreateOdooUser`](crate::steps::create_odoo_user): la home
/// deve risultare identica quale che sia lo step che l'ha consegnata.
const HANDED_OVER_MODE: u32 = 0o750;

/// Crea `ctx.odoo_home` (tipicamente `/opt/odoo`) se non esiste, in modo
/// reversibile.
pub struct PrepareOptRoot {
    ops: Box<dyn SystemOps>,
    /// Stato preesistente della directory, deciso in `snapshot` e confermato a
    /// `CreatedByUs` dopo un `run` che l'ha effettivamente creata.
    prestate: PreState,
}

impl PrepareOptRoot {
    /// Costruttore con `SystemOps` iniettabile (usato dai test con un mock).
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
        // Se la directory esiste già, non è nostra: undo sarà NO-OP.
        // Altrimenti resta `Untracked` finché il run non la crea davvero.
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

        // Preexisting: la directory c'è già e non è nostra → non la tocchiamo
        // (né owner né permessi).
        if self.prestate == PreState::Preexisting {
            info!(dir = %dir.display(), "run: directory già presente, nessuna azione");
            return Ok(());
        }

        if ctx.dry_run {
            info!(dir = %dir.display(), "run (dry-run): creerei la directory (owned root, 0755)");
            return Ok(());
        }

        // Crea solo il livello mancante (non `create_dir_all`): il rollback deve
        // ripristinare esattamente ciò che abbiamo aggiunto, non anche il parent.
        fs::create_dir(dir).map_err(|e| StepError::io(dir, e))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(OPT_ROOT_MODE))
            .map_err(|e| StepError::io(dir, e))?;

        // Da ora è nostra: undo potrà rimuoverla. Il flag va alzato **prima**
        // dell'eventuale consegna: se il chown fallisce, la directory esiste già
        // ed è nostra da rimuovere.
        self.prestate = PreState::CreatedByUs;

        // Se l'utente esiste già, la home è sua da subito: `owned root` era solo
        // l'attesa di uno step successivo, e quello step non farà nulla perché
        // vedrà l'utente `Preexisting` (A-V3-4).
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
        // Invariante: undo agisce SOLO su ciò che abbiamo creato noi.
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (directory non creata da noi)");
            return Ok(());
        }

        let dir = &ctx.odoo_home;

        if ctx.dry_run {
            info!(dir = %dir.display(), "undo (dry-run): rimuoverei la directory");
            return Ok(());
        }

        // Idempotente: se è già sparita, niente da fare.
        if !dir.exists() {
            info!(dir = %dir.display(), "undo: directory già assente");
            return Ok(());
        }

        // Rimuovi SOLO se vuota: mai `rm -rf`. Se contiene artefatti, gli step
        // successivi hanno i propri undo che girano prima (ordine inverso),
        // quindi qui la dir dovrebbe essere vuota.
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
