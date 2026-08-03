//! [`NginxEnableSite`] (13c): abilita il sito e libera la porta 80.
//!
//! # Mutazione insidiosa: il default site
//!
//! Per liberare la porta 80, il Bash rimuove `sites-enabled/default` — che è
//! **config preesistente del cliente**. Qui lo snapshot torna a essere
//! **protezione**: registra *cosa* c'era prima di noi, e l'undo lo
//! **ripristina**. È l'analogo del backup di
//! [`GenerateConfig`](crate::steps::generate_config), esteso alla config Nginx
//! di terzi.
//!
//! # Registrare *se* c'era non basta (A-V3-5)
//!
//! Fino alla R10 lo snapshot era un `bool`: «il default site esisteva?». Da lì
//! due difetti, dallo stesso errore.
//!
//! **Il ripristino non era fedele.** L'undo ricreava sempre un symlink verso
//! `/etc/nginx/sites-available/default`, il target *standard di distribuzione*.
//! Se il cliente aveva un default che puntava altrove — un vhost con un altro
//! nome — la sua config non tornava com'era: tornava com'è *di solito*. Per un
//! progetto che ripristina il `.bashrc` byte-per-byte era un doppio standard.
//!
//! **E si perdeva un file.** `symlink_exists` usa `symlink_metadata`, che
//! risponde `true` anche per un **file regolare**; `remove_symlink` è
//! `fs::remove_file`, che lo cancella. Un amministratore che avesse scritto
//! `sites-enabled/default` come file vero — pratica non rara — si vedeva il
//! contenuto **distrutto** dal `run`, e l'undo gli restituiva un symlink al
//! default della distro. Non un residuo: una perdita di configurazione, nella
//! fase che questo progetto descrive come *«dove vivono le mutazioni su config
//! di terzi»*.
//!
//! Ora la natura si legge con [`SystemOps::path_kind`] e si **persiste**, e ogni
//! natura ha il suo trattamento: un symlink si rimuove e si ricrea verso il
//! target registrato; un file regolare si **sposta in un backup** e si rimette
//! al suo posto; qualunque altra cosa non si tocca.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::{PathKind, SystemOps};

const SITES_AVAILABLE: &str = "/etc/nginx/sites-available";
const SITES_ENABLED: &str = "/etc/nginx/sites-enabled";
/// Target standard Debian/Ubuntu del default site.
///
/// Usato **solo** come ripiego per gli stati persistiti prima della R11, che
/// registravano l'esistenza ma non il target. Per gli stati nuovi il target si
/// rilegge da [`DefaultSite::Symlink`].
const DEFAULT_SITE_TARGET: &str = "/etc/nginx/sites-available/default";

/// Cosa c'era in `sites-enabled/default` prima di noi, e cosa ne abbiamo fatto.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultSite {
    /// Non c'era nulla: niente da rimuovere, niente da ripristinare.
    #[default]
    Assente,
    /// Un symlink. Si rimuove e si ricrea verso **questo** target, non verso
    /// quello che di solito ci sarebbe.
    Symlink { target: std::path::PathBuf },
    /// Un file regolare: contiene configurazione di qualcuno. Non si cancella —
    /// si sposta in un backup, e l'undo lo rimette dov'era.
    ///
    /// `backup` è `None` finché il `run` non l'ha spostato davvero.
    FileRegolare { backup: Option<String> },
    /// Una directory, o un symlink illeggibile: non sappiamo trattarlo, quindi
    /// non lo tocchiamo. La porta 80 potrebbe restare occupata, e lo diciamo.
    Intoccabile,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxEnableSiteSnapshot {
    /// Stato del nostro symlink `sites-enabled/odoo<N>`.
    pub link: PreState,
    /// **Legacy (pre-R11).** Il default site esisteva prima di noi?
    ///
    /// Non è più la fonte di verità — lo è [`Self::default_site`] — ma si
    /// continua a scriverlo e a leggerlo: uno stato persistito da una versione
    /// precedente non ha il campo nuovo, e senza questo ripiego il suo rollback
    /// lascerebbe la porta 80 senza il default site che avevamo tolto. Stessa
    /// cura di retrocompatibilità dell'`InstallConfig` in R4.
    #[serde(default)]
    pub default_site_existed: bool,
    /// Cosa c'era davvero, e cosa ne abbiamo fatto. `None` solo negli stati
    /// scritti prima della R11.
    #[serde(default)]
    pub default_site: Option<DefaultSite>,
}

pub struct NginxEnableSite {
    ops: Box<dyn SystemOps>,
    snap: NginxEnableSiteSnapshot,
}

impl NginxEnableSite {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxEnableSiteSnapshot::default(),
        }
    }

    fn src(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_AVAILABLE}/odoo{}", ctx.odoo_version_short))
    }
    fn link(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_ENABLED}/odoo{}", ctx.odoo_version_short))
    }
    fn default_site() -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_ENABLED}/default"))
    }

    /// Dove finisce il backup di un default site che è un **file regolare**.
    ///
    /// Deliberatamente **fuori** da `sites-enabled/`: `nginx.conf` include
    /// `sites-enabled/*` — ogni file, non solo i `.conf` — quindi un backup
    /// lasciato lì verrebbe caricato lo stesso e la porta 80 resterebbe
    /// occupata. Cioè il difetto che stiamo correggendo, con un nome diverso.
    /// `/etc/nginx/` non è invece oggetto di alcun glob.
    fn default_site_backup() -> String {
        format!("/etc/nginx/default-site.bak.{}", unix_timestamp())
    }

    /// Toglie di mezzo il default site secondo la sua natura, registrando cosa
    /// è stato fatto perché l'undo possa disfarlo esattamente.
    fn displace_default_site(&mut self) -> Result<(), StepError> {
        let path = Self::default_site();
        match self.snap.default_site.clone().unwrap_or_default() {
            DefaultSite::Assente => Ok(()),

            DefaultSite::Symlink { .. } => {
                // Un symlink non ha contenuto proprio: rimuoverlo non distrugge
                // nulla, e il target è registrato per ricrearlo identico.
                if let Err(e) = self.ops.remove_symlink(&path) {
                    warn!(error = %e, "run: rimozione default site fallita, proseguo");
                } else {
                    info!("run: default site nginx rimosso (porta 80 liberata)");
                }
                Ok(())
            }

            DefaultSite::FileRegolare { .. } => {
                // Un file vero contiene configurazione di qualcuno: si sposta,
                // non si cancella. Un `move` fallito è un errore vero — meglio
                // fermarsi che proseguire avendo perso il file a metà.
                let backup = Self::default_site_backup();
                self.ops.move_file(&path, std::path::Path::new(&backup))?;
                self.snap.default_site = Some(DefaultSite::FileRegolare {
                    backup: Some(backup.clone()),
                });
                warn!(
                    backup = %backup,
                    "run: il default site era un FILE, non un symlink: spostato in backup \
                     (l'undo lo rimetterà al suo posto)"
                );
                Ok(())
            }

            DefaultSite::Intoccabile => {
                warn!(
                    path = %path.display(),
                    "run: il default site non è né un symlink né un file regolare: non lo tocco. \
                     Se occupa la porta 80, nginx potrebbe non partire: rimuovilo a mano."
                );
                Ok(())
            }
        }
    }
}

impl NginxEnableSite {
    /// Rimette il default site **com'era**, non com'è di solito.
    ///
    /// Best-effort come ogni undo: un fallimento è un `warn!`, non un errore che
    /// ferma la pulizia degli altri step.
    fn restore_default_site(&self) {
        let path = Self::default_site();
        match &self.snap.default_site {
            Some(DefaultSite::Assente) | Some(DefaultSite::Intoccabile) => {}

            Some(DefaultSite::Symlink { target }) => {
                // Il target registrato, non la costante: se il cliente aveva un
                // default che puntava altrove, torna a puntare *là*.
                if let Err(e) = self.ops.create_symlink(target, &path) {
                    warn!(error = %e, "undo: ripristino default site fallito, proseguo (best-effort)");
                } else {
                    info!(target = %target.display(), "undo: default site nginx ripristinato");
                }
            }

            Some(DefaultSite::FileRegolare {
                backup: Some(backup),
            }) => {
                let backup_path = std::path::Path::new(backup);
                if !self.ops.path_exists(backup_path) {
                    warn!(backup = %backup, "undo: backup del default site non trovato");
                    return;
                }
                if let Err(e) = self.ops.move_file(backup_path, &path) {
                    warn!(error = %e, "undo: ripristino del file default site fallito, proseguo (best-effort)");
                } else {
                    info!("undo: default site nginx (file) rimesso al suo posto");
                }
            }

            // Era un file, ma il `run` non è arrivato a spostarlo: non c'è
            // niente da rimettere ed è giusto così.
            Some(DefaultSite::FileRegolare { backup: None }) => {}

            // Stato persistito prima della R11: sappiamo solo *se* c'era.
            // Si ricade sul comportamento storico — un symlink al target
            // standard — che è il meglio ricavabile da quell'informazione.
            None => {
                if !self.snap.default_site_existed {
                    return;
                }
                let target = std::path::PathBuf::from(DEFAULT_SITE_TARGET);
                warn!(
                    target = %target.display(),
                    "undo: stato senza la natura del default site (pre-R11): ripristino un \
                     symlink al target standard. Se il tuo puntava altrove, verificalo."
                );
                if let Err(e) = self.ops.create_symlink(&target, &path) {
                    warn!(error = %e, "undo: ripristino default site fallito, proseguo (best-effort)");
                }
            }
        }
    }
}

impl Step for NginxEnableSite {
    fn name(&self) -> &str {
        "nginx-enable-site"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxEnableSiteSnapshot::default();
            return Ok(());
        }
        self.snap.link = if self.ops.symlink_exists(&Self::link(ctx)) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        // Informazione chiave per l'undo: cosa c'è al posto del default site.
        // Non "se c'è": *cosa*. Da questa distinzione dipende se il ripristino
        // sarà fedele o soltanto plausibile (A-V3-5).
        let default_site = match self.ops.path_kind(&Self::default_site()) {
            PathKind::Absent => DefaultSite::Assente,
            PathKind::Symlink { target } => DefaultSite::Symlink { target },
            PathKind::RegularFile => DefaultSite::FileRegolare { backup: None },
            PathKind::Other => DefaultSite::Intoccabile,
        };
        self.snap.default_site_existed = default_site != DefaultSite::Assente;
        self.snap.default_site = Some(default_site);
        info!(
            link = ?self.snap.link,
            default_site = ?self.snap.default_site,
            "snapshot nginx-enable-site"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-enable-site");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): rimuoverei il default site e creerei il symlink");
            return Ok(());
        }

        // Libera la porta 80 togliendo di mezzo il default site. *Come* si toglie
        // dipende da cosa è: un symlink si rimuove (è ricreabile identico), un
        // file regolare si sposta (il suo contenuto è di qualcuno).
        self.displace_default_site()?;

        self.ops.create_symlink(&Self::src(ctx), &Self::link(ctx))?;
        if self.snap.link == PreState::Untracked {
            self.snap.link = PreState::CreatedByUs;
        }
        info!("run: sito abilitato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rm nostro symlink + ripristino default site se esisteva");
            return Ok(());
        }

        // Rimuovi il nostro symlink se creato da noi.
        if self.snap.link == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_symlink(&Self::link(ctx)) {
                warn!(error = %e, "undo: rimozione symlink nostro fallita, proseguo (best-effort)");
            }
        }

        self.restore_default_site();
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// `default_site_existed` è l'informazione che permette all'undo di
    /// **ricucire** la config del cliente: senza reidratarla, un rollback da
    /// disco rimuoverebbe il nostro vhost e lascerebbe la porta 80 senza il
    /// default site che avevamo tolto.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
