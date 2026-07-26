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

/// Stato rilevato dei sorgenti Odoo in una directory target.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OdooSourceState {
    /// La directory non esiste.
    #[default]
    Absent,
    /// Clone git presente, sul branch indicato.
    GitRepo { branch: String },
    /// Directory con `odoo-bin` ma senza `.git` (es. estratta da tarball).
    TarballPresent,
    /// Directory esistente ma non valida (né git corretto né `odoo-bin`).
    InvalidDir,
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

    // --- servizi systemd (Fase 5) --------------------------------------------
    fn service_is_enabled(&self, service: &str) -> bool;
    fn service_is_active(&self, service: &str) -> bool;
    fn service_enable(&self, service: &str) -> Result<(), StepError>;
    fn service_disable(&self, service: &str) -> Result<(), StepError>;
    fn service_start(&self, service: &str) -> Result<(), StepError>;
    fn service_stop(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl restart <service>` (riavvia per applicare la nuova config).
    fn service_restart(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), StepError>;

    // --- PostgreSQL (Fase 5) -------------------------------------------------
    /// `true` se il ruolo esiste (`SELECT 1 FROM pg_roles ...`).
    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError>;
    /// `true` se il database esiste (`SELECT 1 FROM pg_database ...`).
    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError>;
    /// Crea il ruolo. `password` è il segreto in chiaro (o `None` = peer auth):
    /// l'escaping e l'invio sicuro (stdin, stderr soppresso) sono qui dentro,
    /// così la password non trapela mai fuori dal confine.
    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError>;
    /// `DROP ROLE IF EXISTS "<role>"`.
    fn pg_drop_role(&self, role: &str) -> Result<(), StepError>;
    /// `createdb --owner <owner> <db>`.
    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError>;
    /// `dropdb --if-exists --force <db>` (chiude le connessioni attive).
    fn dropdb(&self, db: &str) -> Result<(), StepError>;

    // --- sorgenti Odoo (Fase 6, tutto come utente non-root) ------------------
    /// Esegue `sudo -u <user> -- <program> <args>` (privilegio minimo).
    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError>;
    /// `sudo -u <user> -- mkdir -p <path>`.
    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError>;
    /// Rimozione ricorsiva (`rm -rf`) di una dir del **nostro** perimetro
    /// (`<install_dir>/...`). Idempotente: dir assente → no-op.
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError>;
    /// Rileva lo stato dei sorgenti Odoo in `target`.
    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError>;
    /// Un singolo tentativo di `git clone` come `user`.
    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError>;
    /// Fallback: scarica ed estrae il tarball del branch in `target` (come user).
    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError>;
    /// `<venv>/bin/python3` esiste ed è eseguibile?
    fn venv_python_exists(&self, venv: &Path) -> bool;
    /// `python3 -m venv` è disponibile sul sistema?
    fn python_venv_available(&self) -> bool;
    /// `sudo -u <user> -- python3 -m venv <venv>`.
    fn create_venv(&self, user: &str, venv: &Path) -> Result<(), StepError>;
    /// Legge un file di testo (es. requirements.txt).
    fn read_to_string(&self, path: &Path) -> Result<String, StepError>;

    // --- config + init DB (Fase 7) -------------------------------------------
    /// Scrive `content` in un file **privato** (mode `0600`, owned root): la
    /// master password non è mai leggibile da altri utenti in nessun istante.
    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError>;
    /// Sposta `src` su `dst` (rename, con fallback copy+remove cross-device).
    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// Copia `src` in `dst` (per il backup).
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// Rimuove un file. Idempotente (`rm -f`): assente → no-op.
    fn remove_file(&self, path: &Path) -> Result<(), StepError>;
    /// `true` se il DB ha già lo schema Odoo (tabella `ir_module_module`).
    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError>;
    /// `sudo -u <user> -- <python> <odoo_bin> -c <conf> -d <db> -i base
    /// --without-demo=all --stop-after-init`.
    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError>;
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

/// Esegue un comando catturandone lo stdout (per query psql/systemctl).
fn capture_command(program: &str, args: &[&str]) -> Result<String, StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

/// Esegue un comando passando `input` via **stdin** (non in argv).
///
/// Se `secret` è `true`, lo stderr NON viene incluso nell'errore: psql, in caso
/// di errore, ristampa la riga fallita — che conterrebbe la password. Per i
/// comandi che portano segreti si preferisce perdere il dettaglio diagnostico
/// piuttosto che rischiare un leak nei log.
fn run_command_stdin(
    program: &str,
    args: &[&str],
    input: &str,
    secret: bool,
) -> Result<(), StepError> {
    use std::io::Write;
    use std::process::Stdio;

    let rendered = format!("{program} {}", args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;

    // `take()` per chiudere lo stdin (EOF) dopo la scrittura ed evitare deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| StepError::CommandFailed {
                command: rendered.clone(),
                status: "stdin-write-failed".to_string(),
                stderr: e.to_string(),
            })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "wait-failed".to_string(),
            stderr: e.to_string(),
        })?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = if secret {
            "<output soppresso: potrebbe contenere segreti>".to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr,
        })
    }
}

/// Escape di un literal SQL: raddoppia gli apici singoli (standard SQL).
///
/// Usato per la password del ruolo. Gli identifier (nome ruolo/DB) sono
/// validati come identifier in Fase 1 e vengono comunque double-quotati.
pub fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
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

    fn service_is_enabled(&self, service: &str) -> bool {
        matches!(
            Command::new("systemctl").args(["is-enabled", service]).output(),
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "enabled"
        )
    }

    fn service_is_active(&self, service: &str) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", service])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn service_enable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["enable", service])
    }

    fn service_disable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["disable", service])
    }

    fn service_start(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["start", service])
    }

    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["stop", service])
    }

    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["restart", service])
    }

    fn daemon_reload(&self) -> Result<(), StepError> {
        run_command("systemctl", &["daemon-reload"])
    }

    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError> {
        let sql = format!("SELECT 1 FROM pg_roles WHERE rolname = '{}';", escape_sql_literal(role));
        let out = capture_command("sudo", &["-Hiu", "postgres", "--", "psql", "-tAc", &sql])?;
        Ok(!out.trim().is_empty())
    }

    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError> {
        let sql = format!("SELECT 1 FROM pg_database WHERE datname = '{}';", escape_sql_literal(db));
        let out = capture_command("sudo", &["-Hiu", "postgres", "--", "psql", "-tAc", &sql])?;
        Ok(!out.trim().is_empty())
    }

    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError> {
        // Identifier double-quotato; password come literal escaped. L'SQL va via
        // stdin e, in caso di errore, lo stderr è soppresso (potrebbe contenerla).
        let sql = match password {
            Some(pw) => format!(
                "CREATE ROLE \"{role}\" WITH LOGIN CREATEDB PASSWORD '{}';",
                escape_sql_literal(pw)
            ),
            None => format!("CREATE ROLE \"{role}\" WITH LOGIN CREATEDB;"),
        };
        run_command_stdin(
            "sudo",
            &["-Hiu", "postgres", "--", "psql", "-v", "ON_ERROR_STOP=1"],
            &sql,
            /* secret */ true,
        )
    }

    fn pg_drop_role(&self, role: &str) -> Result<(), StepError> {
        let sql = format!("DROP ROLE IF EXISTS \"{role}\";");
        run_command(
            "sudo",
            &["-Hiu", "postgres", "--", "psql", "-v", "ON_ERROR_STOP=1", "-c", &sql],
        )
    }

    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError> {
        run_command(
            "sudo",
            &["-Hiu", "postgres", "--", "createdb", "--owner", owner, db],
        )
    }

    fn dropdb(&self, db: &str) -> Result<(), StepError> {
        run_command(
            "sudo",
            &["-Hiu", "postgres", "--", "dropdb", "--if-exists", "--force", db],
        )
    }

    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError> {
        let mut full = vec!["-u", user, "--", program];
        full.extend_from_slice(args);
        run_command("sudo", &full)
    }

    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError> {
        let p = path.to_string_lossy();
        run_command("sudo", &["-u", user, "--", "mkdir", "-p", &p])
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError> {
        if target.join(".git").is_dir() {
            let target_str = target.to_string_lossy();
            let branch = capture_command(
                "sudo",
                &["-u", user, "--", "git", "-C", &target_str, "rev-parse", "--abbrev-ref", "HEAD"],
            )
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
            return Ok(OdooSourceState::GitRepo { branch });
        }
        if target.is_dir() {
            if target.join("odoo-bin").is_file() {
                return Ok(OdooSourceState::TarballPresent);
            }
            return Ok(OdooSourceState::InvalidDir);
        }
        Ok(OdooSourceState::Absent)
    }

    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError> {
        let target_str = target.to_string_lossy();
        let depth_str = depth.to_string();
        run_command(
            "sudo",
            &[
                "-u", user, "--", "git",
                "-c", "http.version=HTTP/1.1",
                "-c", "core.compression=0",
                "clone", url,
                "--branch", branch,
                "--single-branch",
                "--no-tags",
                "--depth", &depth_str,
                &target_str,
            ],
        )
    }

    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError> {
        let tmp = std::env::temp_dir().join("odoo-src.tar.gz");
        let tmp_str = tmp.to_string_lossy().into_owned();
        let target_str = target.to_string_lossy().into_owned();

        // Scarica; poi crea/estrai come utente (i file risultano owned da lui).
        let outcome = (|| {
            run_command("wget", &["-qO", &tmp_str, url])?;
            run_command("sudo", &["-u", user, "--", "mkdir", "-p", &target_str])?;
            run_command(
                "sudo",
                &["-u", user, "--", "tar", "-xzf", &tmp_str, "-C", &target_str, "--strip-components=1"],
            )?;
            if !target.join("odoo-bin").is_file() {
                return Err(StepError::Precondition(format!(
                    "fallback tarball completato ma odoo-bin assente in {target_str}"
                )));
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp);
        outcome
    }

    fn venv_python_exists(&self, venv: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let python = venv.join("bin").join("python3");
        python
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    fn python_venv_available(&self) -> bool {
        Command::new("python3")
            .args(["-m", "venv", "--help"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn create_venv(&self, user: &str, venv: &Path) -> Result<(), StepError> {
        let venv_str = venv.to_string_lossy();
        run_command("sudo", &["-u", user, "--", "python3", "-m", "venv", &venv_str])
    }

    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        std::fs::read_to_string(path).map_err(|e| StepError::io(path, e))
    }

    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(content.as_bytes())
            .map_err(|e| StepError::io(path, e))
    }

    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        if std::fs::rename(src, dst).is_ok() {
            return Ok(());
        }
        // Fallback cross-device: copia poi rimuove la sorgente.
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        std::fs::remove_file(src).map_err(|e| StepError::io(src, e))?;
        Ok(())
    }

    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError> {
        let sql = "SELECT 1 FROM information_schema.tables \
                   WHERE table_schema='public' AND table_name='ir_module_module';";
        let out = capture_command(
            "sudo",
            &["-Hiu", "postgres", "--", "psql", "-d", db, "-tAc", sql],
        )?;
        Ok(!out.trim().is_empty())
    }

    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError> {
        let python = python.to_string_lossy();
        let odoo_bin = odoo_bin.to_string_lossy();
        let conf = conf.to_string_lossy();
        run_command(
            "sudo",
            &[
                "-u", user, "--", &python, &odoo_bin,
                "-c", &conf,
                "-d", db,
                "-i", "base",
                "--without-demo=all",
                "--stop-after-init",
            ],
        )
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
