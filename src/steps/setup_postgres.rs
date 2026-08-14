//! [`SetupPostgres`]: installs, enables and starts PostgreSQL, reversibly.
//!
//! the case where a single `PreState` is not enough: PostgreSQL has **four
//! orthogonal state axes**, each restored separately (decision D4).
//!
//! # the fourth axis: the cluster (M3)
//!
//! on Debian the package's postinst creates and starts a cluster, so installing
//! is enough. on Fedora it **initialises nothing**, and without an explicit
//! init the service does not start — this step used to fail its final check
//! without explaining why.
//!
//! initialising is a **mutation producing an artifact**, the data directory, so
//! it needs a `PreState` of its own or it would come into existence unrecorded
//! (A-R5-3).
//!
//! an axis and not a new step, because the init must happen **between** the
//! installation and the start, both of which live here — and step names are
//! persisted identifiers, so splitting this one would break the rebuilding of
//! manifests already in the field.
//!
//! decision D3: the undo does **stop + disable**, both reversible, but does
//! **not** purge the package. purging PostgreSQL is too destructive for an
//! automatic rollback on a customer machine, and happens only under
//! `--aggressive-rollback`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const PG_SERVICE: &str = "postgresql";

/// do the two answers about "where is the cluster" conflict? (A-MD-6)
///
/// `ours` is the constant that drives the undo; `declared` is what the unit
/// tells `postgresql-setup`, when it can be read.
///
/// three outcomes: they match, which is the normal case; `declared` is `None`,
/// where **we do not know** and nothing is concluded — blindness is not
/// conflict; or they diverge, and we refuse naming both paths.
///
/// pure: the interesting case needs a machine with a drop-in on
/// `postgresql.service`, which no test has.
pub fn cluster_path_conflict(
    ours: &std::path::Path,
    declared: Option<&std::path::Path>,
) -> Option<String> {
    let declared = declared?;
    if declared == ours {
        return None;
    }
    Some(format!(
        "il cluster PostgreSQL di questo sistema è configurato in `{}`, mentre l'installer sa \
         gestirne uno solo in `{}`. La differenza non è cosmetica: `postgresql-setup` \
         inizializzerebbe il primo, mentre il rollback con --aggressive-rollback rimuoverebbe il \
         secondo — cioè una directory che non abbiamo creato noi e che su questa macchina può \
         contenere un cluster preesistente. Mi fermo prima di toccare qualsiasi cosa. \
         Vie d'uscita: installare su una macchina con il PGDATA di default, oppure rimuovere il \
         drop-in che sposta PGDATA da `postgresql.service` (`systemctl show -p Environment \
         postgresql.service` mostra quello attivo).",
        declared.display(),
        ours.display()
    ))
}

/// snapshot of PostgreSQL's independent state axes.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresSnapshot {
    /// was the package already installed?
    pub installed: PreState,
    /// was the service already enabled?
    pub enabled: PreState,
    /// was the service already running?
    pub active: PreState,
    /// was the cluster already initialised?
    ///
    /// stays `Untracked` on families where the package creates it: nothing to
    /// initialise and nothing to remove.
    ///
    /// `serde(default)` for backward compatibility: a pre-M3 snapshot lacks the
    /// field and reads as `Untracked`, which is the truth for every existing
    /// Debian installation.
    #[serde(default)]
    pub cluster_initialized: PreState,
}

/// installs, enables and starts PostgreSQL, restoring each axis on undo.
pub struct SetupPostgres {
    ops: Box<dyn SystemOps>,
    snap: PostgresSnapshot,
}

impl SetupPostgres {
    /// the packages that install the server, per this family's manager.
    fn packages(&self) -> Vec<String> {
        self.ops.packages().catalog().postgres
    }

    /// the name to ask "is PostgreSQL installed?" with.
    ///
    /// not the first of [`Self::packages`]: a different question, and on
    /// another family a different answer.
    fn marker_package(&self) -> String {
        self.ops.packages().catalog().postgres_marker
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: PostgresSnapshot::default(),
        }
    }
}

impl Step for SetupPostgres {
    fn name(&self) -> &str {
        "setup-postgres"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // per axis: already true before us means `Preexisting` and the undo
        // leaves it; otherwise `Untracked` until `run` makes it ours.
        self.snap.installed = if self.ops.packages().is_installed(&self.marker_package()) {
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
        // the cluster: `Untracked` where the family lacks the concept,
        // otherwise `Preexisting` when the data directory is initialised. the
        // marker is `PG_VERSION`, which `initdb` writes first and which exists
        // **only** in a valid PGDATA — the directory itself can be there,
        // created by the package, with no cluster inside.
        self.snap.cluster_initialized = match self.ops.distro().postgres_data_dir() {
            None => PreState::Untracked,
            Some(dir) => {
                // before reading THAT path: is it really the one the service
                // will use? (A-MD-6) an answer exists only once PostgreSQL is
                // installed, which is exactly when a drop-in can be there.
                // refusing in the snapshot stops before any mutation.
                if let Some(conflitto) = cluster_path_conflict(
                    &dir,
                    self.ops.distro().declared_postgres_data_dir().as_deref(),
                ) {
                    return Err(StepError::Precondition(conflitto));
                }
                if self.ops.path_exists(&dir.join("PG_VERSION")) {
                    PreState::Preexisting
                } else {
                    PreState::Untracked
                }
            }
        };
        info!(
            installed = ?self.snap.installed,
            enabled = ?self.snap.enabled,
            active = ?self.snap.active,
            cluster = ?self.snap.cluster_initialized,
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
            let packages = self.packages();
            let refs: Vec<&str> = packages.iter().map(String::as_str).collect();
            self.ops.packages().install(&refs)?;
            self.snap.installed = PreState::CreatedByUs;
            info!("run: PostgreSQL installato");
        }
        // the cluster BEFORE enabling and starting: without it the service does
        // not come up, and the final check would fail with a message about
        // journalctl rather than the real cause.
        if self.snap.cluster_initialized == PreState::Untracked
            && self.ops.distro().postgres_data_dir().is_some()
        {
            // asked again, and not out of abundance: at snapshot time the
            // package may not have been installed, so the unit did not exist
            // and the question had no answer. this is the last instant we can
            // still refuse without having created a cluster.
            if let Some(dir) = self.ops.distro().postgres_data_dir() {
                if let Some(conflitto) = cluster_path_conflict(
                    &dir,
                    self.ops.distro().declared_postgres_data_dir().as_deref(),
                ) {
                    return Err(StepError::Precondition(conflitto));
                }
            }
            self.ops.distro().init_postgres_cluster()?;
            self.snap.cluster_initialized = PreState::CreatedByUs;
            info!("run: cluster PostgreSQL inizializzato");
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

        // final check: it must come out running.
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

        // order: stop → disable → purge. but the DECISION to purge is taken
        // BEFORE the stop: listing the databases needs a running postgres.
        let purge_wanted = self.snap.installed == PreState::CreatedByUs && ctx.aggressive_rollback;
        let purge_safe = purge_wanted && self.cluster_safe_to_purge(ctx);

        // stop only what we started (D4); an already-running service stays up.
        if self.snap.active == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(PG_SERVICE) {
                warn!(error = %e, "undo: stop postgresql fallito, proseguo (best-effort)");
            }
        }

        // disable only what we enabled (D4).
        if self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_disable(PG_SERVICE) {
                warn!(error = %e, "undo: disable postgresql fallito, proseguo (best-effort)");
            }
        }

        // never purge by default: only under the flag, AND only when the
        // cluster hosts no other databases.
        if self.snap.installed == PreState::CreatedByUs {
            if purge_safe {
                warn!(
                    "--aggressive-rollback: purge PostgreSQL (nessun altro database nel cluster)"
                );
                let packages = self.packages();
                let refs: Vec<&str> = packages.iter().map(String::as_str).collect();
                crate::steps::remove_with_recovery(self.ops.packages(), "setup-postgres", &refs);
                if let Err(e) = self.ops.packages().remove_orphans() {
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

        // the cluster goes only if **we created it**, under the same conditions
        // as the package purge. not excessive caution: a PGDATA holds *every*
        // database of the cluster, not only ours. without the flag it stays —
        // an empty data directory is an inert leftover, somebody else's data is
        // not.
        if self.snap.cluster_initialized == PreState::CreatedByUs {
            match self.ops.distro().postgres_data_dir() {
                Some(dir) if purge_safe => {
                    warn!(
                        data_dir = %dir.display(),
                        "--aggressive-rollback: rimuovo il cluster PostgreSQL che avevamo inizializzato"
                    );
                    if let Err(e) = self.ops.remove_dir_all(&dir) {
                        warn!(
                            data_dir = %dir.display(),
                            error = %e,
                            "undo: rimozione del cluster fallita, proseguo (best-effort)"
                        );
                    }
                }
                Some(dir) => info!(
                    data_dir = %dir.display(),
                    "undo: cluster PostgreSQL lasciato al suo posto (serve --aggressive-rollback, \
                     e solo se non ospita altri database)"
                ),
                None => {}
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// rehydrates the **four axes** together.
    ///
    /// they are independent and each decides a different undo action, so
    /// rehydrating one would leave the others `Untracked` — a rollback that
    /// forgets to turn off what it turned on.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}

impl SetupPostgres {
    /// cluster caution, best-effort: purge only when no database besides ours
    /// and the maintenance one is present. an unobtainable list means **no**
    /// purge.
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
