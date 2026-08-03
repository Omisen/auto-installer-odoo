//! [`SetupPostgres`]: installa, abilita e avvia PostgreSQL, in modo reversibile.
//!
//! È il caso in cui il `PreState` singolo non basta: PostgreSQL ha **quattro assi
//! ortogonali** di stato, ognuno da ripristinare separatamente (decisione D4).
//!
//! # Il quarto asse: il cluster (M3)
//!
//! Su Debian/Ubuntu il postinst di `postgresql` crea e avvia un cluster `main`,
//! quindi installare basta. Su Fedora `postgresql-server` **non inizializza
//! niente**: senza `postgresql-setup --initdb` il servizio non parte, e questo
//! step falliva alla verifica finale senza spiegare perché.
//!
//! L'inizializzazione è una **mutazione che produce un artefatto** — il data
//! directory — quindi non è «un comando in più»: ha bisogno di un `PreState`
//! suo, o sarebbe qualcosa che nasce senza che nessuno lo annoti (A-R5-3).
//!
//! Perché un asse e non uno step nuovo: l'init deve avvenire **fra**
//! l'installazione e lo start, che vivono entrambi qui. Uno step separato
//! costringerebbe a spezzare `setup-postgres` in tre, e i nomi degli step sono
//! identificatori persistiti — spezzarlo romperebbe la ricostruzione dei
//! manifesti già in campo. Il quarto asse è invece il pattern che questo step
//! già usa.
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
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const PG_SERVICE: &str = "postgresql";

/// Snapshot dei tre assi indipendenti dello stato di PostgreSQL.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresSnapshot {
    /// Il pacchetto era già installato?
    pub installed: PreState,
    /// Il servizio era già `enabled`?
    pub enabled: PreState,
    /// Il servizio era già `active` (running)?
    pub active: PreState,
    /// Il cluster (data directory) era già inizializzato?
    ///
    /// Resta `Untracked` sulle famiglie dove il pacchetto lo crea da sé: lì non
    /// c'è nulla da inizializzare e nulla da rimuovere.
    ///
    /// `serde(default)` per retrocompatibilità: uno snapshot scritto prima di M3
    /// non ha il campo e si legge come `Untracked`, che è la verità per ogni
    /// installazione Debian esistente. Stessa cura di `default_site` in R11.
    #[serde(default)]
    pub cluster_initialized: PreState,
}

/// Installa/abilita/avvia PostgreSQL ripristinando ogni asse allo stato iniziale.
pub struct SetupPostgres {
    ops: Box<dyn SystemOps>,
    snap: PostgresSnapshot,
}

impl SetupPostgres {
    /// I pacchetti che installano il server, secondo il gestore di questa
    /// famiglia.
    fn packages(&self) -> Vec<String> {
        self.ops.packages().catalog().postgres
    }

    /// Il nome con cui si chiede «PostgreSQL è installato?».
    ///
    /// Non è il primo elemento di [`Self::packages`]: è una domanda diversa, e
    /// su un'altra famiglia la risposta è un nome diverso.
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
        // Per ciascun asse: se già vero prima di noi → Preexisting (undo lo
        // lascia); altrimenti Untracked (lo faremo noi → CreatedByUs dopo run).
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
        // Il cluster: `Untracked` se la famiglia non ha il concetto, altrimenti
        // `Preexisting` se il data directory è già inizializzato. Il marcatore è
        // `PG_VERSION`, che initdb scrive per primo e che esiste **solo** in un
        // PGDATA valido: la directory può esistere vuota (la crea il pacchetto)
        // senza che ci sia un cluster dentro.
        self.snap.cluster_initialized = match self.ops.distro().postgres_data_dir() {
            None => PreState::Untracked,
            Some(dir) => {
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
        // Il cluster PRIMA di abilitare e avviare: su Fedora senza questo il
        // servizio non parte, e la verifica finale fallirebbe con un messaggio
        // che parla di journalctl invece che della causa vera.
        if self.snap.cluster_initialized == PreState::Untracked
            && self.ops.distro().postgres_data_dir().is_some()
        {
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

        // Il cluster: lo rimuoviamo solo se **l'abbiamo creato noi** e solo alle
        // stesse condizioni del purge del pacchetto (D3-punto2).
        //
        // Non è prudenza eccessiva: un PGDATA contiene *tutti* i database del
        // cluster, non solo il nostro. `cluster_safe_to_purge` è già la domanda
        // giusta — «c'è qualcosa oltre al nostro database?» — e la sua risposta
        // negativa protegge qui esattamente come protegge lì. Senza flag il
        // cluster resta: un data directory vuoto è un residuo inerte, i dati di
        // qualcun altro no.
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

    /// Reidrata i **quattro assi** insieme: installato / abilitato / attivo /
    /// cluster. Sono indipendenti e ognuno decide un'azione diversa dell'undo
    /// (purge, disable, stop, rimozione del data directory); reidratarne solo
    /// uno lascerebbe gli altri a `Untracked`, cioè un rollback che dimentica di
    /// rispegnere ciò che aveva acceso.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
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
