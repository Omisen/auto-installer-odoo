//! [`SetupDataDir`]: crea il `data_dir` di Odoo (il **filestore**) in modo
//! reversibile, con la stessa protezione anti-drop del database.
//!
//! # Perché esiste (A-R5-3)
//!
//! Il `data_dir` configurato in `odoo<N>.conf` è
//! `<odoo_home>/.local/share/Odoo` ([`generate_config::data_dir`]): lì Odoo
//! scrive il filestore, cioè i file veri degli allegati. Finché nessuno step lo
//! creava, quella directory nasceva **da sola** al primo avvio di Odoo, dentro
//! `/opt/odoo` — che l'installer, trovandola già esistente, marca `Preexisting`
//! e non tocca mai. Il risultato, misurato dal job Ubuntu di R5: dopo un
//! rollback completo `/opt/odoo/.local` restava lì.
//!
//! Un artefatto che nasce senza che nessuno lo registri non è annullabile. Qui
//! viene creato da uno step, con il suo `PreState`, e quindi diventa rimovibile
//! per la sola strada che questo progetto ammette: perché *risulta dal
//! `PreState`* che l'abbiamo creato noi.
//!
//! # Il filestore segue il destino del database (anti-drop)
//!
//! Un filestore non è una cache: è la metà su disco dei dati applicativi. Se il
//! database era **preesistente** — un DB del cliente con lo stesso nome, che
//! `CreateDatabase` protegge dal drop — allora il suo filestore contiene
//! allegati veri, che l'installer non ha alcun diritto di cancellare, anche se la
//! directory l'ha materialmente creata lui. Perciò l'undo richiede **due**
//! condizioni: `PreState::CreatedByUs` **e** database creato da noi.
//!
//! Il secondo dato viene letto dal canale `Context::db_created_by_us` durante lo
//! `snapshot` — che gira dopo quello di `CreateDatabase`, quindi il valore c'è —
//! e viene **persistito** nello snapshot di questo step. Non è ridondanza: un
//! rollback da disco ricostruisce il `Context` dalla config persistita, dove
//! quel flag vale `false` di default, e senza copia locale l'undo non saprebbe
//! mai di poter agire. Come per il DB, il verdetto si rilegge, non si rideduce.
//!
//! # Ordine dell'undo, dichiarato
//!
//! Nella sequenza questo step sta dopo `create-database`, quindi il suo undo
//! gira **prima** del `dropdb` (ordine inverso). Se il `dropdb` fallisse — è
//! best-effort — resterebbe un database nostro senza filestore. Non è
//! evitabile senza invertire l'ordine, e invertirlo è impossibile: lo snapshot
//! di questo step deve girare *dopo* quello di `CreateDatabase` per sapere di
//! chi è il database. Il caso è comunque quello di un DB che *stiamo* buttando
//! via, e un `dropdb` fallito finisce già nel report dei residui.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::generate_config;
use crate::system_ops::{RealSystemOps, SystemOps};

/// Snapshot persistito dello step.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDirSnapshot {
    /// Stato del `data_dir` prima di noi.
    pub prestate: PreState,
    /// Il livello **più alto** che mancava sotto `odoo_home` e che quindi
    /// abbiamo creato noi (es. `/opt/odoo/.local`). È ciò che l'undo rimuove: se
    /// il cliente aveva già `.local`, qui c'è `.local/share` o solo il
    /// filestore, e la sua `.local` non viene toccata.
    pub created_root: Option<std::path::PathBuf>,
    /// Il database era stato creato da noi? Condizione anti-drop dell'undo.
    pub db_was_ours: bool,
}

/// Crea il `data_dir` di Odoo (reversibile, gated sull'anti-drop del DB).
pub struct SetupDataDir {
    ops: Box<dyn SystemOps>,
    snap: DataDirSnapshot,
}

impl SetupDataDir {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: DataDirSnapshot::default(),
        }
    }

    /// Il primo livello **inesistente** scendendo da `odoo_home` verso
    /// `data_dir`: la radice di ciò che un `mkdir -p` creerà, e quindi l'unica
    /// cosa che l'undo può rimuovere senza toccare roba di altri.
    ///
    /// `None` se `data_dir` non è sotto `odoo_home` o se esiste già tutto.
    fn highest_missing_level(&self, ctx: &Context) -> Option<std::path::PathBuf> {
        let data_dir = generate_config::data_dir(ctx);
        let relative = data_dir.strip_prefix(&ctx.odoo_home).ok()?;
        let mut current = ctx.odoo_home.clone();
        for component in relative.components() {
            current = current.join(component);
            if !self.ops.path_exists(&current) {
                return Some(current);
            }
        }
        None
    }
}

impl Default for SetupDataDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for SetupDataDir {
    fn name(&self) -> &str {
        "setup-data-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let data_dir = generate_config::data_dir(ctx);

        // Letto ORA: `CreateDatabase::snapshot` l'ha già pubblicato, e da qui in
        // avanti vive nel nostro snapshot persistito (vedi nota di modulo).
        self.snap.db_was_ours = ctx
            .db_created_by_us
            .load(std::sync::atomic::Ordering::SeqCst);

        if self.ops.path_exists(&data_dir) {
            self.snap.prestate = PreState::Preexisting;
            self.snap.created_root = None;
        } else {
            self.snap.prestate = PreState::Untracked;
            self.snap.created_root = self.highest_missing_level(ctx);
        }

        info!(
            data_dir = %data_dir.display(),
            prestate = ?self.snap.prestate,
            created_root = ?self.snap.created_root,
            db_was_ours = self.snap.db_was_ours,
            "snapshot setup-data-dir"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let data_dir = generate_config::data_dir(ctx);

        if self.snap.prestate == PreState::Preexisting {
            info!(
                data_dir = %data_dir.display(),
                "run: filestore già presente, nessuna azione (non è nostro)"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                data_dir = %data_dir.display(),
                "run (dry-run): creerei il data_dir come utente odoo"
            );
            return Ok(());
        }

        // Creato come utente odoo: il filestore deve essere scrivibile dal
        // servizio, e `mkdir -p` copre i livelli intermedi (.local, .local/share).
        self.ops.mkdir_p_as_user(&ctx.odoo_user, &data_dir)?;
        self.snap.prestate = PreState::CreatedByUs;
        info!(
            data_dir = %data_dir.display(),
            "run: data_dir creato (owned {}:{})",
            ctx.odoo_user, ctx.odoo_user
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.snap.prestate,
                "undo NO-OP (data_dir non creato da noi)"
            );
            return Ok(());
        }

        // PROTEZIONE CRITICA: il filestore di un database preesistente contiene
        // allegati del cliente. La directory l'abbiamo creata noi, i dati dentro
        // no.
        if !self.snap.db_was_ours {
            warn!(
                "undo NO-OP: il database era preesistente, il filestore contiene allegati \
                 del cliente e NON viene rimosso (protezione dati cliente)"
            );
            return Ok(());
        }

        let target = self
            .snap
            .created_root
            .clone()
            .unwrap_or_else(|| generate_config::data_dir(ctx));

        // Rete di sicurezza sul path reidratato: un `created_root` fuori da
        // odoo_home (stato corrotto, o scritto da un'altra installazione) non
        // deve diventare un `rm -rf` altrove. Meglio un residuo che un disastro.
        if !target.starts_with(&ctx.odoo_home) || target == ctx.odoo_home {
            warn!(
                target = %target.display(),
                odoo_home = %ctx.odoo_home.display(),
                "undo: path del filestore fuori dal perimetro, non rimuovo nulla"
            );
            return Ok(());
        }

        if ctx.dry_run {
            info!(target = %target.display(), "undo (dry-run): rm -rf del filestore");
            return Ok(());
        }

        // rm -rf del nostro perimetro: la directory è nostra e i dati dentro
        // appartengono a un database che stiamo droppando.
        if let Err(e) = self.ops.remove_dir_all(&target) {
            warn!(
                target = %target.display(),
                error = %e,
                "undo: rm -rf del filestore fallito, proseguo (best-effort)"
            );
        } else {
            info!(target = %target.display(), "undo: filestore rimosso");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// Reidrata `created_root` **e** `db_was_ours`: il primo dice *cosa*
    /// rimuovere, il secondo *se* è lecito. Ricalcolare l'uno o l'altro dopo
    /// l'installazione darebbe la risposta sbagliata in entrambi i casi — le
    /// directory ormai esistono, e il database esiste sia che l'avessimo creato
    /// noi sia che fosse del cliente.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
