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
use crate::step::{decode_snapshot, Step};
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
///
/// I suffissi ammessi sono **solo** quelli per cui la release `0.12.6.1-3`
/// pubblica davvero un `.deb` amd64: `jammy`, `bullseye`, `bookworm`. In
/// particolare **non** esiste un `focal_amd64.deb` in questa release, e
/// `focal` (Ubuntu 20.04) è comunque già rifiutato a monte da
/// [`crate::checks::validate_os`] (richiede Ubuntu ≥ 22.04): non ha quindi un
/// ramo dedicato e, nel caso teorico arrivasse qui, cade nel fallback `jammy`
/// come ogni altro codename sconosciuto.
pub fn map_codename(codename: Option<&str>) -> CodenameMapping {
    let mapped = |s: &str| CodenameMapping {
        suffix: s.to_string(),
        fallback: false,
    };
    match codename {
        // Nessun pacchetto native: jammy è compatibile (mapping esplicito).
        Some("noble") | Some("mantic") | Some("lunar") | Some("jammy") => mapped("jammy"),
        Some("bookworm") => mapped("bookworm"),
        Some("bullseye") => mapped("bullseye"),
        _ => CodenameMapping {
            suffix: "jammy".to_string(),
            fallback: true,
        },
    }
}

/// Pin TOFU: SHA-256 di `wkhtmltox_0.12.6.1-3.jammy_amd64.deb`.
const PIN_JAMMY: &str = "4f723b2691ad8638a9df960e0421d346d7315083e3583a334f33362280ddba15";
/// Pin TOFU: SHA-256 di `wkhtmltox_0.12.6.1-3.bullseye_amd64.deb`.
const PIN_BULLSEYE: &str = "9c687f0c58cf50e01f2a6375d2e34372f8feeec56a84690ea113d298fccadd98";
/// Pin TOFU: SHA-256 di `wkhtmltox_0.12.6.1-3.bookworm_amd64.deb`.
const PIN_BOOKWORM: &str = "98ba0d157b50d36f23bd0dedf4c0aa28c7b0c50fcdcdc54aa5b6bbba81a3941d";

/// Tabella dei checksum SHA-256 attesi, **per suffisso di pacchetto**.
///
/// La chiave è il suffisso del `.deb` che scarichiamo (`jammy`, `bullseye`,
/// `bookworm`), **non** il codename dell'OS dell'utente: è [`map_codename`] a
/// tradurre l'uno nell'altro (es. `noble` → `.deb` `jammy`). La release
/// `0.12.6.1-3` pubblica `.deb` amd64 solo per questi tre suffissi.
///
/// # Natura della garanzia: pinning TOFU (trust-on-first-use)
///
/// La release ufficiale `wkhtmltopdf/packaging` `0.12.6.1-3` **non** pubblica
/// checksum né firme per i `.deb` (upstream costruisce in CI e dichiara che
/// checksum/firme non sono forniti; solo il tag git è firmato GPG). Non esiste
/// quindi un checksum *upstream* da inserire.
///
/// Decisione onesta: **pinning manuale TOFU**. Questi non sono checksum
/// ufficiali, ma pin generati una volta da una fonte fidata: i `.deb` sono
/// stati scaricati via HTTPS dalla release ufficiale su GitHub e ne è stato
/// calcolato lo SHA-256; per `bullseye` e `bookworm` il valore è stato inoltre
/// riscontrato in modo incrociato con una fonte terza indipendente. Da qui in
/// avanti l'installer verifica **ogni** download contro il pin: protegge da
/// mirror compromessi, download corrotti e alterazioni successive — anche senza
/// una firma upstream a garantire il primo scaricamento.
///
/// ## Procedura per (ri)generare i pin
/// ```text
/// for cn in jammy bullseye bookworm; do
///   url="https://github.com/wkhtmltopdf/packaging/releases/download/0.12.6.1-3/wkhtmltox_0.12.6.1-3.${cn}_amd64.deb"
///   echo -n "$cn = "; curl -fsSL "$url" | sha256sum | cut -d' ' -f1
/// done
/// ```
/// Aggiorna i valori **e** questa procedura quando cambi `WK_VERSION`: i pin
/// valgono per una sola versione. Non vanno mai inventati, e la verifica non va
/// mai bypassata né silenziata: un suffisso senza pin fa fallire lo step
/// (fail-closed).
pub fn default_checksums() -> BTreeMap<String, String> {
    [
        ("jammy", PIN_JAMMY),
        ("bullseye", PIN_BULLSEYE),
        ("bookworm", PIN_BOOKWORM),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
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
        // Nome **imprevedibile**, non `<tmp>/wkhtmltox_….deb` (A-V3-3): con un
        // nome fisso e noto, un utente locale poteva piazzare a quel path un
        // symlink verso un file di sistema e farci scrivere sopra da root prima
        // ancora che l'installer partisse.
        //
        // Sul *contenuto* la difesa c'era già ed è quella giusta: il `.deb` viene
        // verificato contro il pin TOFU prima dell'installazione, quindi un file
        // sostituito viene rifiutato. Qui si chiude l'altra metà — dove il file
        // nasce — perché la stessa classe di rischio era già stata giudicata
        // inaccettabile in R1 per il `.conf`, e valeva la coerenza.
        //
        // L'estensione `.deb` va conservata: `apt-get install <file>` riconosce
        // un percorso locale solo da quella.
        let tmp = system_ops::private_temp_path_keeping_extension(&self.tmp_dir, &pkg);

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

        // Installazione via apt sul file locale: risolve le dipendenze di
        // sistema del `.deb` (fontconfig, libxrender1, xfonts-75dpi,
        // xfonts-base) che su una VM minimale non ci sono.
        //
        // Qui c'era `dpkg -i` seguito da `apt-get install -f`. Non funzionava:
        // `dpkg -i` non risolve le dipendenze, esce **1** lasciando il
        // pacchetto `unconfigured`, e il `?` propagava l'errore *prima* di
        // arrivare al fix-broken — che quindi non veniva mai eseguito. Peggio:
        // il dpkg restava rotto, e da lì in poi ogni comando apt falliva,
        // rollback compreso (A-RT-1/A-RT-2, dalla prova reale su Multipass).
        //
        // L'integrità resta garantita: apt installa **questo** file, che
        // abbiamo appena verificato contro il pin TOFU.
        self.ops.apt_install_deb_file(tmp)?;
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

    /// Rimuove **solo** `wkhtmltox`, non le sue dipendenze di sistema.
    ///
    /// L'installazione via apt tira dentro `fontconfig`, `libxrender1`,
    /// `xfonts-75dpi`, `xfonts-base`: librerie e font di sistema, non artefatti
    /// Odoo. Restano installate, per la stessa ragione per cui il rollback
    /// lascia `git`/`curl`/`wget` del bootstrap (D3, decisione 1): sono utility
    /// comuni a bassissimo rischio, e disinstallarle da una macchina cliente
    /// per un rollback farebbe più danni che bene — qualcos'altro potrebbe
    /// averle nel frattempo adottate. Tracciarne un delta separato sarebbe
    /// complessità sproporzionata al rumore che lasciano.
    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (wkhtmltopdf non installato da noi)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry-run): apt purge {WK_PACKAGE}");
            return Ok(());
        }
        crate::steps::purge_with_dpkg_recovery(
            self.ops.as_ref(),
            "install-wkhtmltopdf",
            &[WK_PACKAGE],
        );
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
