//! Preflight checks **non mutanti**: precondizioni verificate prima di qualsiasi
//! step. Nessuna di queste funzioni tocca il sistema — misurano e basta.
//!
//! Sostituiscono `lib/checks.sh` del Bash. Correzione chiave rispetto al Bash
//! (criticità **C4**): `check_disk` non crea più la directory per poterla
//! misurare — misura il primo antenato esistente. La creazione di `/opt/odoo`
//! è ora uno step reversibile ([`crate::steps::prepare_opt_root`]), non un
//! effetto collaterale di un check.
//!
//! I path (`os-release`, target disco) sono **iniettabili**: i test girano
//! senza root e senza toccare `/opt/odoo`.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

use crate::distro::OsFamily;

/// Path di default del file OS release.
pub const OS_RELEASE_PATH: &str = "/etc/os-release";
/// Soglia disco di default (GB).
pub const DEFAULT_MIN_DISK_GB: u64 = 5;
/// Home Odoo di default (target della misura disco).
pub const DEFAULT_DISK_TARGET: &str = "/opt/odoo";

/// Informazioni sull'OS rilevate da `os-release` (servono, es., a wkhtmltopdf).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    pub id: String,
    pub version: String,
    pub codename: Option<String>,
    /// La famiglia a cui `id` appartiene.
    ///
    /// **Non è un ripiego mai**: un `id` di cui non conosciamo la famiglia viene
    /// rifiutato da [`check_os_from`] prima che questa struct esista, quindi
    /// questo campo non contiene mai un valore «di comodo». Vale la pena
    /// notarlo, perché il tipo ha un `Default` — che serve alla
    /// retrocompatibilità del manifesto (vedi [`crate::distro::OsFamily`]) e non
    /// a riempire buchi qui.
    pub family: OsFamily,
}

/// Errore di precondizione (non mutante). I check non hanno `undo`: non mutano.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("questo installer deve essere eseguito come root (EUID atteso 0, trovato {euid}). Riprova con: sudo ...")]
    NotRoot { euid: u32 },

    #[error(
        "esegui via sudo da un utente normale (SUDO_USER assente). \
         Non usare 'sudo -i', 'su -' o login diretto come root"
    )]
    NoSudoUser,

    #[error("file OS release non trovato: {0}. Sistema operativo non riconoscibile")]
    OsReleaseNotFound(PathBuf),

    #[error("impossibile determinare l'OS da {path}: {reason}")]
    OsReleaseParse { path: PathBuf, reason: String },

    #[error(
        "sistema operativo '{id}' non supportato. \
         Supportati: Ubuntu >= 22.04, Debian >= 11, Fedora >= 40"
    )]
    UnsupportedOs { id: String },

    #[error(
        "{id} {version} non supportato. \
         Versione minima: Ubuntu 22.04, Debian 11, Fedora 40"
    )]
    UnsupportedVersion { id: String, version: String },

    #[error("spazio insufficiente su {target}: disponibili {available_gb} GB, richiesti {required_gb} GB")]
    InsufficientDisk {
        target: PathBuf,
        available_gb: u64,
        required_gb: u64,
    },

    #[error("impossibile misurare lo spazio disco su {path}: {reason}")]
    DiskProbe { path: PathBuf, reason: String },

    #[error("porta {port} già in uso: liberala prima di procedere")]
    PortInUse { port: u16 },

    #[error(
        "comando di sistema obbligatorio mancante: {command}. \
         Serve un sistema con systemd e il gestore di pacchetti della sua famiglia \
         (apt-get su Debian/Ubuntu, dnf su Fedora)"
    )]
    MissingCommand { command: String },
}

// --- Root / sudo -------------------------------------------------------------

/// Logica pura: verifica che l'EUID sia 0. Testabile senza essere root.
pub fn ensure_root_euid(euid: u32) -> Result<(), CheckError> {
    if euid == 0 {
        Ok(())
    } else {
        Err(CheckError::NotRoot { euid })
    }
}

/// Stiamo girando come root? Domanda, non verifica: nessun errore, nessun log.
///
/// Serve al `--dry-run`, che è lecito lanciare senza privilegi ma che in quel
/// caso vede meno (A-V3-11).
pub fn running_as_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Verifica che l'installer giri come root.
pub fn check_root() -> Result<(), CheckError> {
    let euid = nix::unistd::geteuid().as_raw();
    ensure_root_euid(euid)?;
    info!("✔ esecuzione come root confermata");
    Ok(())
}

/// Logica pura: `SUDO_USER` deve essere presente e non vuoto.
pub fn ensure_sudo_user(value: Option<&str>) -> Result<(), CheckError> {
    match value {
        Some(user) if !user.is_empty() => Ok(()),
        _ => Err(CheckError::NoSudoUser),
    }
}

/// Verifica che l'installer sia lanciato via `sudo` da un utente normale.
pub fn check_sudo_user() -> Result<(), CheckError> {
    let value = std::env::var("SUDO_USER").ok();
    ensure_sudo_user(value.as_deref())?;
    info!(sudo_user = ?value, "✔ esecuzione via sudo confermata");
    Ok(())
}

/// Check sul **chiamante**: chi sta eseguendo l'installer.
///
/// Separati dai check d'ambiente perché vanno fatti in un momento diverso
/// (A-R9-1): questi rispondono a *chi sei*, e la risposta serve subito — il
/// manifesto dell'installazione è `0600 root`, e senza questi controlli chi lo
/// legge senza privilegi si vede un «permission denied» su un file di cui non sa
/// nulla. I check d'ambiente rispondono invece a *la macchina è adatta*, e vanno
/// fatti solo dopo aver stabilito che l'installazione debba avvenire.
pub fn check_caller() -> Result<(), CheckError> {
    check_root()?;
    check_sudo_user()
}

// --- OS ----------------------------------------------------------------------

/// Estrae il valore di una chiave da un file `os-release`, togliendo gli apici.
fn os_release_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(strip_quotes(v.trim()));
            }
        }
    }
    None
}

/// Rimuove una sola coppia di apici che avvolga l'intero valore.
fn strip_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// Legge e valida l'OS da un file `os-release` (path iniettabile per i test).
pub fn check_os_from(path: &Path) -> Result<OsInfo, CheckError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CheckError::OsReleaseNotFound(path.to_path_buf())
        } else {
            CheckError::OsReleaseParse {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }
        }
    })?;

    let id = os_release_value(&content, "ID")
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| CheckError::OsReleaseParse {
            path: path.to_path_buf(),
            reason: "chiave ID assente".to_string(),
        })?;
    let version =
        os_release_value(&content, "VERSION_ID").ok_or_else(|| CheckError::OsReleaseParse {
            path: path.to_path_buf(),
            reason: "chiave VERSION_ID assente".to_string(),
        })?;
    let codename = os_release_value(&content, "VERSION_CODENAME");

    // La famiglia è il **primo** gate, e l'unico posto in cui si decide se una
    // distribuzione ci sia nota: se `from_os_id` non la riconosce, non c'è
    // nessuna famiglia da scrivere in `OsInfo` e non c'è nulla da validare.
    // Tenere questa decisione in un solo punto evita che la lista delle
    // distribuzioni note viva in due posti che possono divergere — `validate_os`
    // si occupa **solo** delle soglie di versione.
    let family =
        OsFamily::from_os_id(&id).ok_or_else(|| CheckError::UnsupportedOs { id: id.clone() })?;

    let info = OsInfo {
        id,
        version,
        codename,
        family,
    };
    validate_os(&info)?;
    Ok(info)
}

/// L'`ID` dichiarato da un file `os-release`, **senza validarlo**.
///
/// Esiste separata da [`check_os_from`] perché il rollback deve poter girare
/// anche su un sistema su cui *rifiuteremmo di installare*: disinstallare
/// un'istanza non richiede che la macchina sia ancora adatta a ospitarla, e far
/// passare il rollback da una validazione lo renderebbe impossibile proprio dove
/// serve. Serve solo a decidere se **avvisare** di una discordanza con il
/// manifesto (`distro::family_mismatch`), mai a decidere un'azione.
pub fn os_id_from(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    os_release_value(&content, "ID").map(|s| s.to_ascii_lowercase())
}

/// L'ultima Ubuntu su cui la CI di integrazione installa davvero.
pub const NEWEST_TESTED_UBUNTU: (u32, u32) = (24, 4);
/// L'ultima Debian su cui la CI di integrazione gira davvero (job `container`).
pub const NEWEST_TESTED_DEBIAN: (u32, u32) = (12, 0);
/// L'ultima Fedora su cui la CI di integrazione gira davvero.
///
/// Il job `fedora` di `integration.yml` esegue il ciclo completo — installazione,
/// servizio attivo, rollback — in un container con systemd come PID 1. Come per
/// le altre due famiglie, questa costante **insegue la matrice del workflow**, e
/// un test lo rende obbligatorio invece che auspicabile: se divergessero,
/// l'avviso di [`is_newer_than_tested`] mentirebbe in una delle due direzioni.
pub const NEWEST_TESTED_FEDORA: (u32, u32) = (41, 0);

/// La release è **più recente** dell'ultima che abbiamo davvero provato?
///
/// Le soglie di [`validate_os`] sono aperte verso l'alto, e devono restarci: un
/// rifiuto senza prova blocca il caso buono, e un'installazione impedita è un
/// danno certo mentre quello evitato è ipotetico (la lezione di A5.1-bis). Ma
/// «accettiamo» non deve voler dire «tacciamo»: chi installa su Ubuntu 26.04 o
/// Debian 13 ha diritto di sapere che quella release non è fra quelle su cui
/// giriamo la CI, perché è l'informazione che gli serve quando qualcosa va
/// storto (A5.3).
///
/// Pura: la soglia superiore si verifica senza avere quell'OS sotto mano.
pub fn is_newer_than_tested(id: &str, version: &str) -> bool {
    let (major, minor) = parse_version(version);
    let (tested_major, tested_minor) = match id {
        "ubuntu" => NEWEST_TESTED_UBUNTU,
        "debian" => NEWEST_TESTED_DEBIAN,
        "fedora" => NEWEST_TESTED_FEDORA,
        // Qui non ci arriva nulla d'altro: un `ID` di famiglia ignota è già
        // stato rifiutato da `OsFamily::from_os_id`. Inventargli una soglia
        // sarebbe un ramo che non può eseguire.
        _ => return false,
    };
    (major, minor) > (tested_major, tested_minor)
}

/// Applica le soglie di versione minima: Ubuntu ≥ 22.04, Debian ≥ 11.
///
/// **Non** decide se la distribuzione ci sia nota: quello lo ha già fatto
/// [`OsFamily::from_os_id`] dentro [`check_os_from`], che è l'unico gate. Qui il
/// `match` sulla famiglia è esaustivo per costruzione, quindi non c'è nessun
/// ramo «distribuzione sconosciuta» che in produzione non potrebbe mai eseguire.
///
/// La soglia Fedora è **40**, ed è una scelta prudente più che misurata: è la più
/// vecchia release ancora supportata da upstream al momento in cui il backend dnf
/// è stato scritto, e sotto quella nessuno ha provato nulla. Come per le altre
/// famiglie la soglia è aperta verso l'alto, con l'avviso di
/// [`is_newer_than_tested`], che su Fedora tace fino alla 41 compresa: la CI di
/// integrazione esegue il ciclo completo su `fedora:41`, in due scenari (con e
/// senza nginx), quindi [`NEWEST_TESTED_FEDORA`] descrive una release davvero
/// provata e non una speranza.
pub fn validate_os(info: &OsInfo) -> Result<(), CheckError> {
    let (major, minor) = parse_version(&info.version);
    match info.family {
        OsFamily::Debian => {
            let too_old = if info.id == "ubuntu" {
                major < 22 || (major == 22 && minor < 4)
            } else {
                major < 11
            };
            if too_old {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
        OsFamily::Fedora => {
            if major < 40 {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
    }
}

/// Estrae `(major, minor)` da una stringa versione tipo `"22.04"` o `"12"`.
/// Componenti mancanti o non numeriche valgono 0.
fn parse_version(version: &str) -> (u32, u32) {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// Verifica l'OS leggendo il path di default (`/etc/os-release`).
pub fn check_os() -> Result<OsInfo, CheckError> {
    let info = check_os_from(Path::new(OS_RELEASE_PATH))?;
    info!(
        os = %info.id,
        version = %info.version,
        codename = ?info.codename,
        "✔ OS supportato"
    );
    if is_newer_than_tested(&info.id, &info.version) {
        warn!(
            os = %info.id,
            version = %info.version,
            "questa release è più recente di quelle su cui l'installer viene provato \
             (Ubuntu 24.04, Debian 12): l'installazione prosegue, ma nomi di pacchetti e \
             pacchetto wkhtmltopdf potrebbero non essere quelli giusti. Se qualcosa non \
             torna, è il primo posto dove guardare."
        );
    }
    Ok(info)
}

// --- Disco (NON mutante: C4 corretta) ---------------------------------------

/// Risale al primo antenato **esistente** di `path` (al limite `/`).
/// Non crea nulla: è il fix di C4 (misurare senza dover creare la directory).
fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

/// Verifica lo spazio libero sul filesystem di `target`, **senza creare**
/// `target`: se non esiste, misura il primo antenato esistente.
pub fn check_disk(target: &Path, required_gb: u64) -> Result<(), CheckError> {
    let measure = nearest_existing_ancestor(target);

    let stat =
        nix::sys::statvfs::statvfs(measure.as_path()).map_err(|e| CheckError::DiskProbe {
            path: measure.clone(),
            reason: e.to_string(),
        })?;

    // Spazio disponibile all'utente non privilegiato: blocchi * frammento.
    let available_bytes =
        (stat.blocks_available() as u64).saturating_mul(stat.fragment_size() as u64);
    let available_gb = available_bytes / (1024 * 1024 * 1024);

    info!(
        target = %target.display(),
        measured_on = %measure.display(),
        available_gb,
        required_gb,
        "verifica spazio disco"
    );

    if available_gb < required_gb {
        return Err(CheckError::InsufficientDisk {
            target: target.to_path_buf(),
            available_gb,
            required_gb,
        });
    }
    Ok(())
}

// --- Porte -------------------------------------------------------------------

/// Esito della verifica di una porta.
#[derive(Debug, PartialEq, Eq)]
pub enum PortStatus {
    Free,
    InUse,
    /// Nessuno strumento disponibile (ss/netstat/lsof): non bloccante.
    Unknown,
}

/// Verifica che le porte richieste siano libere: `odoo_port` e, se
/// `with_nginx`, anche 80 e 443. Una porta `Unknown` è trattata come libera
/// (warning non bloccante, come nel Bash).
pub fn check_ports(
    odoo_port: u16,
    with_nginx: bool,
    nginx_already_serving: bool,
) -> Result<(), CheckError> {
    for port in ports_to_check(odoo_port, with_nginx, nginx_already_serving) {
        match probe_port(port) {
            PortStatus::Free => info!(port, "✔ porta disponibile"),
            PortStatus::Unknown => {
                warn!(
                    port,
                    "impossibile verificare la porta (ss/netstat/lsof assenti): assumo libera"
                )
            }
            PortStatus::InUse => return Err(CheckError::PortInUse { port }),
        }
    }
    Ok(())
}

/// Quali porte vanno controllate. Decisione **pura**, separata dalla sonda.
///
/// Separata perché il caso interessante — «nginx già in ascolto sulla 80» — non
/// è riproducibile in un test: dipende da cosa gira sulla macchina che esegue i
/// test, e su una macchina dove la 80 è libera un controllo sbagliato passerebbe
/// lo stesso. Con la scelta delle porte come valore di ritorno, la regola si
/// verifica in entrambe le direzioni e senza dipendere dall'ambiente. Stesso
/// motivo di `ensure_root_euid` e `state::trust_verdict`.
pub fn ports_to_check(odoo_port: u16, with_nginx: bool, nginx_already_serving: bool) -> Vec<u16> {
    let mut ports = vec![odoo_port];
    // 80 e 443 si controllano solo se dovranno essere prese da un nginx che
    // **non sta già servendo** (A-V3-15).
    //
    // Il controllo pretendeva la 80 libera ogni volta che si chiedeva
    // `--with-nginx`. Ma lo scenario supportato — e quello che gli step nginx
    // gestiscono esplicitamente, con `NginxInstall` che marca `Preexisting` un
    // nginx già installato e attivo e non lo tocca — è proprio *aggiungere un
    // vhost a un nginx che sta già girando*. Su quella macchina la 80 è occupata
    // **da nginx**, cioè dal programma che stiamo per configurare, e rifiutare
    // l'installazione significava rendere impossibile il caso d'uso normale:
    // un reverse proxy esistente a cui si aggiunge Odoo.
    //
    // È la stessa distinzione di `InstallState::owns_the_http_port`: una porta
    // occupata da noi non è un conflitto. Se invece nginx non sta servendo e la
    // 80 è presa, il conflitto è reale (Apache, un altro proxy) e il controllo
    // deve dire di no — perché lì nginx non riuscirebbe nemmeno a fare il bind.
    if with_nginx && !nginx_already_serving {
        ports.push(80);
        ports.push(443);
    } else if with_nginx {
        info!("nginx è già in ascolto: le porte 80/443 sono sue, nessun conflitto da verificare");
    }
    ports
}

/// Sonda una porta con cascata `ss → netstat → lsof`.
fn probe_port(port: u16) -> PortStatus {
    if command_exists("ss") {
        if let Some(out) = capture(Command::new("ss").args(["-lntuH"])) {
            return classify_listing(&out, port);
        }
    }
    if command_exists("netstat") {
        if let Some(out) = capture(Command::new("netstat").args(["-lntu"])) {
            return classify_listing(&out, port);
        }
    }
    if command_exists("lsof") {
        let arg = format!("-iTCP:{port}");
        if let Some(out) = capture(Command::new("lsof").args([arg.as_str(), "-sTCP:LISTEN"])) {
            return if out.trim().is_empty() {
                PortStatus::Free
            } else {
                PortStatus::InUse
            };
        }
    }
    PortStatus::Unknown
}

/// Cerca `:PORT` seguito da spazio nell'output di ss/netstat.
fn classify_listing(listing: &str, port: u16) -> PortStatus {
    let needle = format!(":{port} ");
    if listing.lines().any(|line| line.contains(&needle)) {
        PortStatus::InUse
    } else {
        PortStatus::Free
    }
}

/// Esegue un comando catturandone lo stdout; `None` se non eseguibile.
fn capture(cmd: &mut Command) -> Option<String> {
    cmd.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

// --- Comandi -----------------------------------------------------------------

/// `true` se `command` esiste ed è eseguibile in una delle dir di `PATH`.
/// Non esegue il comando: scandisce solo il filesystem.
fn command_exists(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(command);
        candidate
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// I comandi che **devono** esserci, per questa famiglia.
///
/// Puro: la scelta si verifica senza avere quei comandi sulla macchina che
/// esegue i test — e senza la separazione sarebbe verificabile solo su una
/// Fedora vera, cioè in nessun test.
///
/// # Perché è il primo punto che un'esecuzione su Fedora incontrava
///
/// Fino a M2 questa lista conteneva `apt-get` **per nome**, quindi su Fedora
/// l'installazione si fermava qui — prima di ogni altra cosa — con un messaggio
/// che diceva «serve un sistema Debian/Ubuntu». Il gestore di pacchetti è per
/// definizione ciò che cambia fra famiglie: chiederne uno per nome è la stessa
/// classe di errore del `match os_id` che questo lavoro sta smontando.
pub fn required_commands(family: OsFamily) -> [&'static str; 2] {
    match family {
        OsFamily::Debian => ["apt-get", "systemctl"],
        OsFamily::Fedora => ["dnf", "systemctl"],
    }
}

/// Verifica i prerequisiti di sistema **non installabili** dallo script: il
/// gestore di pacchetti della famiglia e `systemctl`. `nginx`/`certbot` sono
/// opzionali (solo info).
pub fn check_commands(family: OsFamily) -> Result<(), CheckError> {
    for command in required_commands(family) {
        if command_exists(command) {
            info!(command, "✔ presente");
        } else {
            return Err(CheckError::MissingCommand {
                command: command.to_string(),
            });
        }
    }
    for command in ["nginx", "certbot"] {
        if command_exists(command) {
            info!(command, "✔ presente");
        } else {
            info!(
                command,
                "ℹ opzionale, non trovato (installabile se necessario)"
            );
        }
    }
    Ok(())
}
