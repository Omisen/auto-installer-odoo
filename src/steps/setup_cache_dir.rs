//! [`SetupCacheDir`]: possiede `<odoo_home>/.cache`, così il rollback può
//! rimuoverla.
//!
//! # Perché esiste (A-R5-3, seconda metà)
//!
//! `/opt/odoo` è l'`$HOME` dell'utente `odoo`, ed è la directory che l'installer
//! — trovandola già esistente — marca `Preexisting` e non svuota mai. Ma
//! *dentro* quella home girano parecchi programmi per conto nostro: `pip`,
//! `odoo-bin`, il servizio stesso. Tutti scrivono in `$HOME/.cache`, che è dove
//! su Linux le cache vanno a finire.
//!
//! R6 aveva chiuso il caso più grosso spostando la cache di `pip` dentro il venv
//! (`--cache-dir`). Non bastava: il job Ubuntu 22.04 di R6-hotfix-2, che è il
//! primo ad arrivare **in fondo** all'installazione, ha trovato di nuovo
//! `/opt/odoo/.cache` dopo un rollback completo. Con l'installazione intera girano
//! anche `odoo-bin -i base` e il servizio Odoo, e i produttori di cache si
//! moltiplicano: lo scan dei font di fontconfig, il `selfcheck` delle versioni di
//! `pip` (che nelle versioni fino alla 23 ignora `--cache-dir` e scrive comunque
//! nella cache utente), e chiunque altro domani.
//!
//! Inseguire i produttori uno per uno è una battaglia che si perde: sono
//! programmi di terzi e cambiano comportamento fra versioni. Qui si cambia
//! domanda — non «chi ha scritto in `.cache`?» ma «di chi **è** `.cache`?». Se
//! la creiamo noi è nostra, e il rollback la rimuove; se c'era già è del
//! cliente, e non si tocca. Il numero di produttori diventa irrilevante.
//!
//! # Rapporto con `SetupDataDir`
//!
//! Stessa meccanica (possedere un ramo dentro la home altrui), **gate diverso**:
//! il filestore contiene dati applicativi e la sua rimozione è subordinata anche
//! alla proprietà del database. Una cache no: è rigenerabile per definizione, e
//! l'unica domanda è chi ha creato la directory. Nessuna condizione in più.
//!
//! # Posizione nella sequenza
//!
//! Presto: subito dopo `setup-log-dir`, prima che qualunque cosa giri come
//! utente `odoo`. Lo snapshot deve vedere la home **prima** che i produttori di
//! cache la tocchino, altrimenti troverebbe una `.cache` "preesistente" che
//! preesistente non è.
//!
//! Il bonus è l'ordine dell'undo. Gli undo girano al contrario, quindi essere
//! presto qui significa essere **tardi** là: la cache viene rimossa dopo che il
//! servizio è stato fermato, il venv cancellato e i sorgenti rimossi — cioè dopo
//! che ogni possibile scrittore ha smesso di scrivere. È l'opposto del compromesso
//! che `setup-data-dir` deve accettare (vedi la sua nota sull'ordine).

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// Nome della directory cache nella home dell'utente `odoo`.
const CACHE_SUBDIR: &str = ".cache";

/// Snapshot persistito dello step.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheDirSnapshot {
    /// Stato di `<odoo_home>/.cache` prima di noi.
    pub prestate: PreState,
    /// Il livello più alto che mancava e che quindi abbiamo creato noi. Per
    /// `.cache` coincide sempre con la directory stessa (è un solo livello), ma
    /// resta persistito perché è l'undo a decidere cosa rimuovere, e deve
    /// rileggerlo invece di ricalcolarlo.
    pub created_root: Option<std::path::PathBuf>,
}

/// Crea (e quindi possiede) `<odoo_home>/.cache`.
pub struct SetupCacheDir {
    ops: Box<dyn SystemOps>,
    snap: CacheDirSnapshot,
}

impl SetupCacheDir {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: CacheDirSnapshot::default(),
        }
    }

    /// `<odoo_home>/.cache`.
    pub fn cache_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.odoo_home.join(CACHE_SUBDIR)
    }
}

impl Step for SetupCacheDir {
    fn name(&self) -> &str {
        "setup-cache-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let cache = Self::cache_dir(ctx);
        if self.ops.path_exists(&cache) {
            self.snap.prestate = PreState::Preexisting;
            self.snap.created_root = None;
        } else {
            self.snap.prestate = PreState::Untracked;
            self.snap.created_root =
                crate::steps::highest_missing_level(self.ops.as_ref(), &ctx.odoo_home, &cache);
        }
        info!(
            cache = %cache.display(),
            prestate = ?self.snap.prestate,
            "snapshot setup-cache-dir"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let cache = Self::cache_dir(ctx);

        if self.snap.prestate == PreState::Preexisting {
            info!(
                cache = %cache.display(),
                "run: la cache esisteva già, non è nostra (né la usiamo, né la rimuoveremo)"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(cache = %cache.display(), "run (dry-run): creerei la cache come utente odoo");
            return Ok(());
        }

        // Creata come utente `odoo`: chi ci scriverà è lui, non root. Se la
        // creassimo root i programmi lanciati come odoo non potrebbero usarla,
        // e ne aprirebbero un'altra da qualche altra parte.
        self.ops.mkdir_p_as_user(&ctx.odoo_user, &cache)?;
        self.snap.prestate = PreState::CreatedByUs;
        info!(cache = %cache.display(), "run: cache creata (owned {}:{})", ctx.odoo_user, ctx.odoo_user);
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.snap.prestate,
                "undo NO-OP (cache non creata da noi)"
            );
            return Ok(());
        }

        // Nessun gate oltre al PreState: è una cache, il suo contenuto è
        // rigenerabile e non appartiene a nessun database. L'unica domanda che
        // conta l'abbiamo già fatta nello snapshot.
        let target = self
            .snap
            .created_root
            .clone()
            .unwrap_or_else(|| Self::cache_dir(ctx));
        crate::steps::remove_created_root(
            self.ops.as_ref(),
            self.name(),
            &ctx.odoo_home,
            &target,
            ctx.dry_run,
        );
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
