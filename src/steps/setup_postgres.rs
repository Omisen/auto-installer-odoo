//! [`SetupPostgres`]: installa, abilita e avvia PostgreSQL, in modo reversibile.
//!
//! È il caso in cui il `PreState` singolo non basta: PostgreSQL ha **tre assi
//! ortogonali** di stato, ognuno da ripristinare separatamente (decisione D4).
//!
//! Decisione ferma D3-punto2: l'undo di default fa **stop + disable** (entrambi
//! reversibili) ma **NON purga** il pacchetto — il purge di PostgreSQL è troppo
//! distruttivo per un rollback automatico su macchina cliente (rischio dati).
//! Il purge avviene solo con `--aggressive-rollback`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

const PG_SERVICE: &str = "postgresql";
/// Pacchetto usato come marker di "installato".
const PG_MARKER_PACKAGE: &str = "postgresql";
const PG_PACKAGES: &[&str] = &["postgresql", "postgresql-contrib"];

/// Snapshot dei tre assi indipendenti dello stato di PostgreSQL.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresSnapshot {
    /// Il pacchetto era già installato?
    pub installed: PreState,
    /// Il servizio era già `enabled`?
    pub enabled: PreState,
    /// Il servizio era già `active` (running)?
    pub active: PreState,
}

/// Installa/abilita/avvia PostgreSQL ripristinando ogni asse allo stato iniziale.
pub struct SetupPostgres {
    ops: Box<dyn SystemOps>,
    snap: PostgresSnapshot,
}

impl SetupPostgres {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: PostgresSnapshot::default(),
        }
    }
}

impl Default for SetupPostgres {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for SetupPostgres {
    fn name(&self) -> &str {
        "setup-postgres"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // Per ciascun asse: se già vero prima di noi → Preexisting (undo lo
        // lascia); altrimenti Untracked (lo faremo noi → CreatedByUs dopo run).
        self.snap.installed = if self.ops.dpkg_is_installed(PG_MARKER_PACKAGE) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.enabled = if self.ops.service_is_enabled(PG_SERVICE) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.active = if self.ops.service_is_active(PG_SERVICE) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(
            installed = ?self.snap.installed,
            enabled = ?self.snap.enabled,
            active = ?self.snap.active,
            "snapshot setup-postgres"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry-run): installerei/abiliterei/avvierei PostgreSQL secondo necessità");
            return Ok(());
        }

        if self.snap.installed == PreState::Untracked {
            self.ops.apt_install(PG_PACKAGES)?;
            self.snap.installed = PreState::CreatedByUs;
            info!("run: PostgreSQL installato");
        }
        if self.snap.enabled == PreState::Untracked {
            self.ops.service_enable(PG_SERVICE)?;
            self.snap.enabled = PreState::CreatedByUs;
            info!("run: servizio postgresql abilitato");
        }
        if self.snap.active == PreState::Untracked {
            self.ops.service_start(PG_SERVICE)?;
            self.snap.active = PreState::CreatedByUs;
            info!("run: servizio postgresql avviato");
        }

        // Verifica finale: deve risultare attivo.
        if !self.ops.service_is_active(PG_SERVICE) {
            return Err(StepError::Precondition(
                "PostgreSQL non risulta attivo dopo lo start (controlla journalctl -u postgresql)"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): stop/disable secondo lo snapshot; purge solo se aggressive");
            return Ok(());
        }

        // Ordine: stop → disable → (eventuale purge). Ma la DECISIONE di purgare
        // va presa PRIMA dello stop: elencare i DB richiede un postgres attivo.
        let purge_wanted = self.snap.installed == PreState::CreatedByUs && ctx.aggressive_rollback;
        let purge_safe = purge_wanted && self.cluster_safe_to_purge(ctx);

        // active: fermo solo se l'avevamo avviato noi (D4). Se era già attivo,
        // lo lasciamo running.
        if self.snap.active == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(PG_SERVICE) {
                warn!(error = %e, "undo: stop postgresql fallito, proseguo (best-effort)");
            }
        }

        // enabled: disabilito solo se l'avevamo abilitato noi (D4).
        if self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_disable(PG_SERVICE) {
                warn!(error = %e, "undo: disable postgresql fallito, proseguo (best-effort)");
            }
        }

        // installed: NON purgare di default (troppo distruttivo). Solo con flag,
        // E solo se il cluster non ospita altri database (cautela cluster).
        if self.snap.installed == PreState::CreatedByUs {
            if purge_safe {
                warn!(
                    "--aggressive-rollback: purge PostgreSQL (nessun altro database nel cluster)"
                );
                crate::steps::purge_with_dpkg_recovery(
                    self.ops.as_ref(),
                    "setup-postgres",
                    PG_PACKAGES,
                );
                if let Err(e) = self.ops.apt_autoremove() {
                    warn!(error = %e, "undo: autoremove fallito, proseguo (best-effort)");
                }
            } else if purge_wanted {
                warn!(
                    "PostgreSQL ospita altri database (o non verificabile): NON lo rimuovo per \
                     sicurezza. Applicati solo stop+disable."
                );
            } else {
                info!(
                    "undo: PostgreSQL lasciato installato (stop+disable sono reversibili; \
                     il purge no). Usa --aggressive-rollback per rimuoverlo."
                );
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }
}

impl SetupPostgres {
    /// Cautela cluster (best-effort): purga solo se nel cluster non c'è alcun
    /// database oltre al nostro (`ctx.db_name`) e a quello di manutenzione
    /// `postgres`. Se l'elenco non è ottenibile → **non** purgare (fail-safe).
    fn cluster_safe_to_purge(&self, ctx: &Context) -> bool {
        match self.ops.pg_list_databases() {
            Ok(dbs) => {
                let others: Vec<&String> = dbs
                    .iter()
                    .filter(|d| d.as_str() != ctx.db_name && d.as_str() != "postgres")
                    .collect();
                if others.is_empty() {
                    true
                } else {
                    warn!(others = ?others, "cluster PostgreSQL con altri database: purge declinato");
                    false
                }
            }
            Err(e) => {
                warn!(error = %e, "impossibile elencare i database: per sicurezza non purgo");
                false
            }
        }
    }
}
