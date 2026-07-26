//! [`SystemOps`]: confine sui comandi di sistema privilegiati.
//!
//! Gli step che creano utenti/gruppi o cambiano owner/permessi non chiamano
//! direttamente `useradd`/`chown`: passano da questo trait. In produzione
//! [`RealSystemOps`] esegue i comandi reali; nei test un mock registra *quale*
//! operazione verrebbe eseguita (con quali argomenti, in quale ramo `PreState`)
//! senza toccare il sistema e senza richiedere root.
//!
//! È il modo per soddisfare la testabilità richiesta dalla Fase 3 senza
//! modificare il trait [`crate::step::Step`].

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::StepError;

/// Owner numerico di un path (uid/gid), serializzabile per la persistenza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerId {
    pub uid: u32,
    pub gid: u32,
}

/// Specifica per la creazione di un utente di sistema (argomenti di `useradd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub name: String,
    pub home: PathBuf,
    pub system: bool,
    pub create_home: bool,
    pub user_group: bool,
    pub shell: String,
}

/// Operazioni di sistema mutanti/privilegiate, dietro un confine testabile.
///
/// **Nota di sicurezza:** `delete_user` NON rimuove mai la home (nessun `-r`).
/// La rimozione della home `/opt/odoo` è di competenza esclusiva dello step
/// che l'ha creata (`PrepareOptRoot`), che gira dopo nell'ordine inverso.
pub trait SystemOps {
    fn user_exists(&self, user: &str) -> bool;
    fn path_exists(&self, path: &Path) -> bool;
    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError>;
    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError>;

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError>;
    /// Rimuove l'utente (e il suo gruppo primario). **Mai** `-r`: la home non è
    /// di competenza di questo comando.
    fn delete_user(&self, user: &str) -> Result<(), StepError>;
    fn delete_group(&self, group: &str) -> Result<(), StepError>;

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError>;
    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError>;
    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError>;
    fn mkdir(&self, path: &Path) -> Result<(), StepError>;
    fn rmdir(&self, path: &Path) -> Result<(), StepError>;

    // --- apt / dpkg (Fase 4) -------------------------------------------------
    /// `true` se il pacchetto risulta `install ok installed` a `dpkg-query`.
    fn dpkg_is_installed(&self, pkg: &str) -> bool;
    /// `apt-get install -y --no-install-recommends <pkgs>` (idempotente).
    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError>;
    /// `apt-get purge -y <pkgs>`.
    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError>;
    /// `apt-get autoremove -y`.
    fn apt_autoremove(&self) -> Result<(), StepError>;
    /// `apt-get install -f -y` (risolve dipendenze rotte dopo `dpkg -i`).
    fn apt_fix_broken(&self) -> Result<(), StepError>;
    /// `dpkg -i <path>`.
    fn dpkg_install_file(&self, path: &Path) -> Result<(), StepError>;
    /// Versione di `wkhtmltopdf` installata (es. `"0.12.6.1"`), o `None`.
    fn wkhtmltopdf_version(&self) -> Option<String>;
}

/// Implementazione reale: esegue i comandi di sistema.
#[derive(Debug, Default)]
pub struct RealSystemOps;

impl RealSystemOps {
    pub fn new() -> Self {
        Self
    }
}

/// Esegue un comando esterno (con eventuali env) mappando l'esito su
/// [`StepError::CommandFailed`].
fn run_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().map_err(|e| StepError::CommandFailed {
        command: rendered.clone(),
        status: "spawn-failed".to_string(),
        stderr: e.to_string(),
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Esegue un comando esterno senza env aggiuntivi.
fn run_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    run_command_with_env(program, args, &[])
}

/// Esegue `apt-get` con l'ambiente non-interattivo (niente prompt tzdata /
/// needrestart), come il Bash originale.
fn run_apt(args: &[&str]) -> Result<(), StepError> {
    run_command_with_env(
        "apt-get",
        args,
        &[
            ("DEBIAN_FRONTEND", "noninteractive"),
            ("NEEDRESTART_MODE", "a"),
        ],
    )
}

/// Converte un `Errno` di nix in `io::Error` per allegarlo a `StepError::Io`.
fn errno_io(e: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}

impl SystemOps for RealSystemOps {
    fn user_exists(&self, user: &str) -> bool {
        nix::unistd::User::from_name(user)
            .ok()
            .flatten()
            .is_some()
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|e| StepError::io(path, e))?;
        Ok(OwnerId {
            uid: meta.uid(),
            gid: meta.gid(),
        })
    }

    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError> {
        let mut entries = std::fs::read_dir(path).map_err(|e| StepError::io(path, e))?;
        Ok(entries.next().is_none())
    }

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError> {
        let home = spec.home.to_string_lossy().into_owned();
        let mut args: Vec<String> = Vec::new();
        if spec.system {
            args.push("--system".to_string());
        }
        if spec.create_home {
            args.push("--create-home".to_string());
        }
        args.push("--home-dir".to_string());
        args.push(home);
        if spec.user_group {
            args.push("--user-group".to_string());
        }
        args.push("--shell".to_string());
        args.push(spec.shell.clone());
        args.push(spec.name.clone());

        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_command("useradd", &refs)
    }

    fn delete_user(&self, user: &str) -> Result<(), StepError> {
        // MAI `-r`: la home è di competenza di PrepareOptRoot.undo.
        run_command("userdel", &[user])
    }

    fn delete_group(&self, group: &str) -> Result<(), StepError> {
        run_command("groupdel", &[group])
    }

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError> {
        let uid = nix::unistd::User::from_name(owner)
            .ok()
            .flatten()
            .map(|u| u.uid)
            .ok_or_else(|| {
                StepError::Precondition(format!("utente '{owner}' non trovato per chown"))
            })?;
        let gid = nix::unistd::Group::from_name(group)
            .ok()
            .flatten()
            .map(|g| g.gid)
            .ok_or_else(|| {
                StepError::Precondition(format!("gruppo '{group}' non trovato per chown"))
            })?;
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError> {
        let uid = nix::unistd::Uid::from_raw(id.uid);
        let gid = nix::unistd::Gid::from_raw(id.gid);
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| StepError::io(path, e))
    }

    fn mkdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::create_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn rmdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::remove_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn dpkg_is_installed(&self, pkg: &str) -> bool {
        match Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", pkg])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).contains("install ok installed")
            }
            _ => false,
        }
    }

    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut args = vec!["install", "-y", "--no-install-recommends"];
        args.extend_from_slice(pkgs);
        run_apt(&args)
    }

    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut args = vec!["purge", "-y"];
        args.extend_from_slice(pkgs);
        run_apt(&args)
    }

    fn apt_autoremove(&self) -> Result<(), StepError> {
        run_apt(&["autoremove", "-y"])
    }

    fn apt_fix_broken(&self) -> Result<(), StepError> {
        run_apt(&["install", "-f", "-y"])
    }

    fn dpkg_install_file(&self, path: &Path) -> Result<(), StepError> {
        let rendered = path.to_string_lossy();
        run_command("dpkg", &["-i", &rendered])
    }

    fn wkhtmltopdf_version(&self) -> Option<String> {
        let out = Command::new("wkhtmltopdf").arg("--version").output().ok()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Primo token che sembra una versione (inizia con cifra, ≥ 2 punti).
        text.split_whitespace()
            .find(|tok| {
                tok.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && tok.matches('.').count() >= 2
            })
            .map(|s| s.to_string())
    }
}

/// Confine per i download di rete, separato da [`SystemOps`] così è mockabile
/// nei test senza toccare la rete.
pub trait Downloader {
    /// Scarica `url` in `dest`. La verifica di integrità (checksum) è a carico
    /// del chiamante (vedi [`sha256_hex`]): il download NON è fidato di per sé.
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError>;
}

/// Downloader reale via `wget` (già presente tra i prerequisiti bootstrap).
#[derive(Debug, Default)]
pub struct RealDownloader;

impl RealDownloader {
    pub fn new() -> Self {
        Self
    }
}

impl Downloader for RealDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError> {
        let rendered = dest.to_string_lossy();
        run_command("wget", &["-q", "-O", &rendered, url])
    }
}

/// Calcola lo SHA-256 di un file come stringa esadecimale minuscola.
pub fn sha256_hex(path: &Path) -> Result<String, StepError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| StepError::io(path, e))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| StepError::io(path, e))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
