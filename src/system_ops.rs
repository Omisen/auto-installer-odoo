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
}

/// Implementazione reale: esegue i comandi di sistema.
#[derive(Debug, Default)]
pub struct RealSystemOps;

impl RealSystemOps {
    pub fn new() -> Self {
        Self
    }
}

/// Esegue un comando esterno mappando l'esito su [`StepError::CommandFailed`].
fn run_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let output = Command::new(program).args(args).output().map_err(|e| {
        StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        }
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
}
