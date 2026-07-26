//! [`InstallWkhtmltopdf`]: installa la build wkhtmltopdf patchata Qt
//! (`0.12.6.1-3`) da GitHub releases, **con verifica del checksum SHA-256**
//! prima di installare (gap G3 — il Bash originale non lo fa).
//!
//! Il pacchetto apt di distribuzione (senza Qt patch) genera PDF difettosi con
//! Odoo; serve la build ufficiale del progetto wkhtmltopdf.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{self, Downloader, RealDownloader, RealSystemOps, SystemOps};

/// Versione pinnata (tag della release GitHub).
const WK_VERSION: &str = "0.12.6.1-3";
/// Versione riportata da `wkhtmltopdf --version` quando è quella giusta.
const WK_INSTALLED_MARKER: &str = "0.12.6.1";
/// Nome del pacchetto per il purge in undo.
const WK_PACKAGE: &str = "wkhtmltox";

/// Mappa un codename OS al suffisso di pacchetto wkhtmltopdf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodenameMapping {
    pub suffix: String,
    /// `true` se il codename non è mappato e si usa il fallback `jammy`.
    pub fallback: bool,
}

/// Mappa codename → suffisso pacchetto (fallback `jammy` per gli sconosciuti).
pub fn map_codename(codename: Option<&str>) -> CodenameMapping {
    let mapped = |s: &str| CodenameMapping {
        suffix: s.to_string(),
        fallback: false,
    };
    match codename {
        // Nessun pacchetto native: jammy è compatibile (mapping esplicito).
        Some("noble") | Some("mantic") | Some("lunar") | Some("jammy") => mapped("jammy"),
        Some("focal") => mapped("focal"),
        Some("bookworm") => mapped("bookworm"),
        Some("bullseye") => mapped("bullseye"),
        _ => CodenameMapping {
            suffix: "jammy".to_string(),
            fallback: true,
        },
    }
}

/// Tabella dei checksum SHA-256 attesi, per suffisso di pacchetto.
///
/// # Natura della garanzia: pinning TOFU (trust-on-first-use)
///
/// La release ufficiale `wkhtmltopdf/packaging` `0.12.6.1-3` **non** pubblica
/// checksum né firme per i `.deb` (upstream costruisce in CI e dichiara che
/// checksum/firme non sono forniti; solo il tag git è firmato GPG). Non esiste
/// quindi un checksum *upstream* da inserire.
///
/// Decisione onesta: **pinning manuale TOFU**. Questi non sono checksum
/// ufficiali, ma pin generati una volta da una fonte fidata (HTTPS, sito
/// ufficiale). Da lì l'installer verifica ogni download contro il pin:
/// protegge da mirror compromessi, download corrotti e alterazioni successive
/// — anche senza una firma upstream a garantire il primo scaricamento.
///
/// ## Procedura per (ri)generare i pin
/// ```text
/// for cn in jammy focal bookworm bullseye; do
///   url="https://github.com/wkhtmltopdf/packaging/releases/download/0.12.6.1-3/wkhtmltox_0.12.6.1-3.${cn}_amd64.deb"
///   curl -fsSL "$url" | sha256sum        # inserisci il valore per ${cn} qui sotto
/// done
/// ```
/// Aggiorna questi valori quando cambi la versione pinnata.
///
/// **Stato attuale: tabella vuota** → il meccanismo fail-closed **rifiuta**
/// l'installazione finché i pin non sono inseriti (comportamento onesto e
/// testato). I valori reali li fornisce chi installa, scaricando i `.deb`: NON
/// vanno inventati. La verifica non va mai bypassata né silenziata.
pub fn default_checksums() -> BTreeMap<String, String> {
    // Esempio (PLACEHOLDER da sostituire con pin reali):
    //   ("jammy".to_string(), "<sha256 del .deb jammy>".to_string()),
    BTreeMap::new()
}

/// Installa wkhtmltopdf con verifica checksum, in modo reversibile.
pub struct InstallWkhtmltopdf {
    ops: Box<dyn SystemOps>,
    downloader: Box<dyn Downloader>,
    checksums: BTreeMap<String, String>,
    tmp_dir: PathBuf,
    prestate: PreState,
}

impl InstallWkhtmltopdf {
    pub fn new() -> Self {
        Self::with_parts(
            Box::new(RealSystemOps::new()),
            Box::new(RealDownloader::new()),
            default_checksums(),
            std::env::temp_dir(),
        )
    }

    /// Costruttore con dipendenze iniettabili (usato dai test).
    pub fn with_parts(
        ops: Box<dyn SystemOps>,
        downloader: Box<dyn Downloader>,
        checksums: BTreeMap<String, String>,
        tmp_dir: PathBuf,
    ) -> Self {
        Self {
            ops,
            downloader,
            checksums,
            tmp_dir,
            prestate: PreState::Untracked,
        }
    }

    /// Scarica, **verifica il checksum**, installa. Pulisce sempre il temp.
    fn download_verify_install(&self, ctx: &Context) -> Result<(), StepError> {
        let codename = ctx.os_info.as_ref().and_then(|os| os.codename.as_deref());
        let mapping = map_codename(codename);
        if mapping.fallback {
            warn!(codename = ?codename, "codename non mappato: uso il pacchetto jammy come fallback");
        }
        let suffix = &mapping.suffix;

        // G3: senza un checksum atteso non si installa (verifica non bypassabile).
        let expected = self.checksums.get(suffix).ok_or_else(|| {
            StepError::Precondition(format!(
                "checksum wkhtmltopdf non disponibile per '{suffix}' (G3): \
                 impossibile verificare l'integrità, installazione rifiutata"
            ))
        })?;

        let pkg = format!("wkhtmltox_{WK_VERSION}.{suffix}_amd64.deb");
        let url = format!(
            "https://github.com/wkhtmltopdf/packaging/releases/download/{WK_VERSION}/{pkg}"
        );
        let tmp = self.tmp_dir.join(&pkg);

        // Scarica → verifica → installa; il temp va pulito comunque.
        let outcome = self.download_verify_install_inner(&url, &tmp, expected);
        if let Err(e) = std::fs::remove_file(&tmp) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(tmp = %tmp.display(), error = %e, "pulizia file temporaneo fallita");
            }
        }
        outcome
    }

    fn download_verify_install_inner(
        &self,
        url: &str,
        tmp: &Path,
        expected: &str,
    ) -> Result<(), StepError> {
        info!(url, tmp = %tmp.display(), "download wkhtmltopdf");
        self.downloader.download(url, tmp)?;

        let actual = system_ops::sha256_hex(tmp)?;
        if actual != *expected {
            // G3: checksum non combacia → NON installare.
            return Err(StepError::Precondition(format!(
                "checksum wkhtmltopdf non valido (G3): atteso {expected}, calcolato {actual}. \
                 Installazione annullata."
            )));
        }
        info!("checksum wkhtmltopdf verificato");

        self.ops.dpkg_install_file(tmp)?;
        // dpkg -i può lasciare dipendenze non risolte: apt-get install -f le sistema.
        self.ops.apt_fix_broken()?;
        Ok(())
    }
}

impl Default for InstallWkhtmltopdf {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for InstallWkhtmltopdf {
    fn name(&self) -> &str {
        "install-wkhtmltopdf"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // Idempotenza: se la versione corretta è già presente, non è nostra.
        self.prestate = match self.ops.wkhtmltopdf_version() {
            Some(version) if version.starts_with(WK_INSTALLED_MARKER) => PreState::Preexisting,
            _ => PreState::Untracked,
        };
        info!(prestate = ?self.prestate, "snapshot install-wkhtmltopdf");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!("run: wkhtmltopdf {WK_INSTALLED_MARKER} già presente, skip");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): scaricherei, verificherei il checksum e installerei wkhtmltopdf");
            return Ok(());
        }

        self.download_verify_install(ctx)?;
        self.prestate = PreState::CreatedByUs;
        info!("run: wkhtmltopdf installato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (wkhtmltopdf non installato da noi)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry-run): apt purge {WK_PACKAGE}");
            return Ok(());
        }
        if let Err(e) = self.ops.apt_purge(&[WK_PACKAGE]) {
            warn!(error = %e, "undo: purge wkhtmltox fallito, proseguo (best-effort)");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }
}
