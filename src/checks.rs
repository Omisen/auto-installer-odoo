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

    #[error("sistema operativo '{id}' non supportato. Supportati: Ubuntu >= 22.04, Debian >= 11")]
    UnsupportedOs { id: String },

    #[error("{id} {version} non supportato. Versione minima: Ubuntu 22.04, Debian 11")]
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

    #[error("comando di sistema obbligatorio mancante: {command}. Serve un sistema Debian/Ubuntu con apt-get e systemd")]
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

    let info = OsInfo {
        id,
        version,
        codename,
    };
    validate_os(&info)?;
    Ok(info)
}

/// Applica le soglie di versione minima: Ubuntu ≥ 22.04, Debian ≥ 11.
pub fn validate_os(info: &OsInfo) -> Result<(), CheckError> {
    let (major, minor) = parse_version(&info.version);
    match info.id.as_str() {
        "ubuntu" => {
            if major < 22 || (major == 22 && minor < 4) {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
        "debian" => {
            if major < 11 {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
        _ => Err(CheckError::UnsupportedOs {
            id: info.id.clone(),
        }),
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

    let stat = nix::sys::statvfs::statvfs(measure.as_path()).map_err(|e| CheckError::DiskProbe {
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
pub fn check_ports(odoo_port: u16, with_nginx: bool) -> Result<(), CheckError> {
    let mut ports = vec![odoo_port];
    if with_nginx {
        ports.push(80);
        ports.push(443);
    }

    for port in ports {
        match probe_port(port) {
            PortStatus::Free => info!(port, "✔ porta disponibile"),
            PortStatus::Unknown => {
                warn!(port, "impossibile verificare la porta (ss/netstat/lsof assenti): assumo libera")
            }
            PortStatus::InUse => return Err(CheckError::PortInUse { port }),
        }
    }
    Ok(())
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

/// Verifica i prerequisiti di sistema **non installabili** dallo script:
/// `apt-get` e `systemctl`. `nginx`/`certbot` sono opzionali (solo info).
pub fn check_commands() -> Result<(), CheckError> {
    for command in ["apt-get", "systemctl"] {
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
            info!(command, "ℹ opzionale, non trovato (installabile se necessario)");
        }
    }
    Ok(())
}
