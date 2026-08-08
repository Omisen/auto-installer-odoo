//! [`InitializeOdooDatabase`]: inizializza lo schema base di Odoo nel database.
//!
//! # Protezione critica: hard-stop su init di un DB preesistente
//!
//! Gemella dell'anti-drop di Fase 5. Le due regole insieme garantiscono che un
//! DB con dati reali del cliente non venga né **cancellato** né **alterato**:
//!
//! - anti-drop (Fase 5): non distruggere dati altrui;
//! - hard-stop (qui): non **scrivere** in dati altrui.
//!
//! L'installer RIFIUTA di eseguire `-i base` su un database che non ha creato
//! lui. Scrivere lo schema Odoo dentro un DB preesistente non ha undo pulito,
//! quindi la difesa è **non farlo affatto**. L'informazione "il DB è nostro?"
//! arriva da `CreateDatabase` via [`Context::db_created_by_us`] (default `false`
//! = rifiuta).
//!
//! # C2 — init non atomico, undo no-op
//!
//! L'init non è atomico: se muore a metà, il DB resta in stato intermedio. Ma
//! non si tenta alcuna riparazione incrementale: poiché l'init gira **solo** su
//! DB `CreatedByUs`, la pulizia dello schema è coperta dal `dropdb` di
//! `CreateDatabase` (che gira dopo, in ordine inverso). Quindi l'undo qui è un
//! **NO-OP documentato**: si butta e si ricrea il DB pulito.

use std::sync::atomic::Ordering;

use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VENV_SUBDIR: &str = "sandbox";
const REPO_SUBDIR: &str = "odoo";

/// Inizializza lo schema base di Odoo (solo su DB nostro).
pub struct InitializeOdooDatabase {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl InitializeOdooDatabase {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }

    fn python_bin(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir
            .join(VENV_SUBDIR)
            .join("bin")
            .join("python3")
    }
    fn odoo_bin(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(REPO_SUBDIR).join("odoo-bin")
    }
    fn conf(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir
            .join(format!("odoo{}.conf", ctx.odoo_version_short))
    }
}

impl Step for InitializeOdooDatabase {
    fn name(&self) -> &str {
        "initialize-odoo-database"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        // Schema già presente? → nulla da fare (idempotente), indipendentemente
        // da chi possiede il DB.
        if self.ops.pg_db_initialized(&ctx.db_name)? {
            self.prestate = PreState::Preexisting;
            info!(db = %ctx.db_name, "snapshot: schema Odoo già presente");
            return Ok(());
        }

        // Schema assente. HARD-STOP se il DB non è nostro: non si scrive lo
        // schema Odoo in un database preesistente del cliente.
        //
        // Il comportamento resta invariato — l'hard-stop è la protezione
        // corretta, e l'installer *non* può distinguere da solo "residuo nostro"
        // da "database del cliente": in questa esecuzione il DB esiste e basta.
        // Ma l'utente può, e da R4 ha anche lo strumento: se il residuo viene da
        // un'installazione precedente non completata, il suo file di stato è
        // ancora sul disco e `invok rollback` lo consuma, rimuovendo
        // esattamente ciò che quella run aveva creato — e nient'altro (A3.3).
        if !ctx.db_created_by_us.load(Ordering::SeqCst) {
            return Err(StepError::Precondition(format!(
                "Il database '{db}' esisteva già prima dell'installazione; \
                 l'inizializzazione dello schema Odoo è rifiutata per non alterare \
                 dati preesistenti. Usa un nome DB diverso o un DB vuoto creato \
                 dall'installer.\n\
                 Se '{db}' è il residuo di un'installazione precedente non completata \
                 (e non un database con dati reali), ripulisci quella installazione con \
                 `sudo invok rollback`: legge lo stato lasciato da quella \
                 esecuzione e rimuove solo ciò che aveva creato lei. In alternativa \
                 rimuovi il database a mano — `sudo -u postgres dropdb {db}` — oppure \
                 scegli un nome diverso.",
                db = ctx.db_name
            )));
        }

        // DB nostro, schema assente → procederemo.
        self.prestate = PreState::Untracked;
        info!(db = %ctx.db_name, "snapshot: DB nostro, schema da inizializzare");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!(db = %ctx.db_name, "run: schema già presente, skip init");
            return Ok(());
        }
        if ctx.dry_run {
            info!(db = %ctx.db_name, "run (dry-run): odoo-bin -i base --without-demo=all --stop-after-init");
            return Ok(());
        }

        // Init come utente odoo (non root).
        self.ops.odoo_init_base(
            &ctx.odoo_user,
            &Self::python_bin(ctx),
            &Self::odoo_bin(ctx),
            &Self::conf(ctx),
            &ctx.db_name,
        )?;

        // Verifica post-init: lo schema deve ora essere presente.
        if !self.ops.pg_db_initialized(&ctx.db_name)? {
            return Err(StepError::Precondition(format!(
                "inizializzazione del database '{}' fallita: schema base non rilevato",
                ctx.db_name
            )));
        }

        self.prestate = PreState::CreatedByUs;
        info!(db = %ctx.db_name, "run: schema base inizializzato");
        Ok(())
    }

    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        // NO-OP deliberato (C2): l'init gira solo su DB CreatedByUs; la pulizia
        // dello schema è coperta dal dropdb di CreateDatabase, che gira dopo in
        // ordine inverso. Nessuna riparazione incrementale di uno stato a metà.
        info!(
            "undo NO-OP: lo schema vive in un DB CreatedByUs; la sua rimozione è \
             coperta dall'undo di CreateDatabase (dropdb dell'intero DB)"
        );
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    /// Reidratato per simmetria, benché l'`undo` sia un NO-OP: il contratto
    /// `snapshot_value` ⇄ `rehydrate` vale per tutti gli step, così l'invariante
    /// è verificabile uniformemente e un futuro undo non nascerà con uno stato
    /// vuoto senza che nessuno se ne accorga.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
