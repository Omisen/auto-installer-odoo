//! [`WriteControlScript`] (14a): installa il comando helper `odoo`.
//!
//! Gli artefatti appartengono a **`SUDO_USER`** (chi ha lanciato `sudo`), non a
//! `odoo` né a root, e sono installati **solo per quell'utente** (`~/.local/bin`,
//! non `/usr/local/bin`) — scelta di sicurezza del Bash originale, da preservare.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::SystemOps;

/// Template dello script di controllo (segnaposto sostituiti senza `format!`).
const CONTROL_SCRIPT_TEMPLATE: &str = r#"#!/usr/bin/env bash

set -euo pipefail

SERVICE_NAME="__SERVICE__"
ODOO_OS_USER="__OSUSER__"
usage() {
  echo "Usage: odoo {start|stop|restart|dev|status}"
}

case "${1:-}" in
  start)
    sudo systemctl start "${SERVICE_NAME}"
    ;;
  stop)
    sudo systemctl stop "${SERVICE_NAME}"
    ;;
  restart)
    sudo systemctl restart "${SERVICE_NAME}"
    ;;
  dev)
    sudo systemctl stop "${SERVICE_NAME}"
    sudo su - "${ODOO_OS_USER}" -s /bin/bash
    ;;
  status)
    sudo systemctl status "${SERVICE_NAME}"
    ;;
  *)
    usage
    ;;
esac
"#;

const SCRIPT_MODE: u32 = 0o755;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ControlScriptSnapshot {
    pub script: PreState,
    pub symlink: PreState,
    /// Le directory esistevano prima di noi? (rimozione solo se create da noi).
    pub scripts_dir_existed: bool,
    pub localbin_dir_existed: bool,
    /// Backup di uno script **preesistente**, se ce n'era uno da mettere da
    /// parte prima di riscriverlo (A-V3-9). `None` in ogni altro caso.
    #[serde(default)]
    pub script_backup: Option<String>,
}

pub struct WriteControlScript {
    ops: Box<dyn SystemOps>,
    snap: ControlScriptSnapshot,
}

impl WriteControlScript {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: ControlScriptSnapshot::default(),
        }
    }

    /// Risolve `(SUDO_USER, home)` o un errore chiaro.
    fn user_and_home(&self, ctx: &Context) -> Result<(String, std::path::PathBuf), StepError> {
        let user = ctx.sudo_user.clone().ok_or_else(|| {
            StepError::Precondition(
                "SUDO_USER non disponibile: l'installer deve girare via sudo".to_string(),
            )
        })?;
        let home = self.ops.getent_home(&user)?.ok_or_else(|| {
            StepError::Precondition(format!("impossibile determinare la home per '{user}'"))
        })?;
        Ok((user, std::path::PathBuf::from(home)))
    }
}

/// Genera il contenuto dello script di controllo.
pub fn control_script_content(service: &str, os_user: &str) -> String {
    CONTROL_SCRIPT_TEMPLATE
        .replace("__SERVICE__", service)
        .replace("__OSUSER__", os_user)
}

fn paths(
    home: &std::path::Path,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let scripts_dir = home.join(".scripts");
    let script = scripts_dir.join("odoo.sh");
    let localbin = home.join(".local").join("bin");
    let symlink = localbin.join("odoo");
    (scripts_dir, script, localbin, symlink)
}

impl Step for WriteControlScript {
    fn name(&self) -> &str {
        "write-control-script"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let (_user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home);

        self.snap.script = if self.ops.path_exists(&script) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.symlink = if self.ops.symlink_exists(&symlink) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.scripts_dir_existed = self.ops.path_exists(&scripts_dir);
        self.snap.localbin_dir_existed = self.ops.path_exists(&localbin);
        info!(script = ?self.snap.script, symlink = ?self.snap.symlink, "snapshot write-control-script");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let (user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home);

        if ctx.dry_run {
            info!("run (dry-run): scriverei odoo.sh + symlink per l'utente {user}");
            return Ok(());
        }

        // Directory create come l'utente installatore.
        self.ops.mkdir_p_as_user(&user, &scripts_dir)?;
        self.ops.mkdir_p_as_user(&user, &localbin)?;

        // Lo script si riscrive **sempre** (A-V3-9).
        //
        // Il suo contenuto è generato da noi e porta dentro il nome del servizio:
        // saltarlo perché "esiste già" significava che, reinstallando una
        // versione diversa di Odoo, l'helper `odoo` continuava a pilotare il
        // servizio **vecchio** — `SERVICE_NAME=odoo17` mentre gira `odoo18` — e
        // l'utente lo scopriva solo quando `odoo restart` non faceva quello che
        // doveva. Il `PreState` qui non protegge un contenuto altrui: protegge
        // la *decisione di rimuoverlo all'undo*, ed è per quello che resta.
        //
        // Se però quel file c'era già, non lo si distrugge: si mette da parte e
        // l'undo lo rimette. Stesso trattamento del vhost nginx e dell'odoo.conf
        // — un file preesistente con quel nome potrebbe essere di qualcun altro,
        // e non abbiamo modo di saperlo.
        if self.snap.script == PreState::Preexisting && self.snap.script_backup.is_none() {
            let backup = format!("{}.bak.{}", script.display(), unix_timestamp());
            self.ops.copy_file(&script, std::path::Path::new(&backup))?;
            self.snap.script_backup = Some(backup.clone());
            warn!(backup = %backup, "run: control-script esistente, backup creato");
        }
        let service = format!("odoo{}", ctx.odoo_version_short);
        let content = control_script_content(&service, &ctx.odoo_user);
        self.ops.write_private_file(&script, &content)?;
        self.ops.chmod(&script, SCRIPT_MODE)?;
        if self.snap.script == PreState::Untracked {
            self.snap.script = PreState::CreatedByUs;
        }

        // Symlink (se non già nostro).
        if self.snap.symlink != PreState::Preexisting {
            self.ops.create_symlink(&script, &symlink)?;
            self.snap.symlink = PreState::CreatedByUs;
        }

        // Tutto appartiene a SUDO_USER (non odoo, non root).
        self.ops.chown_to_user(&script, &user)?;
        self.ops.chown_to_user(&symlink, &user)?;
        self.ops.chown_to_user(&scripts_dir, &user)?;
        self.ops.chown_to_user(&localbin, &user)?;

        info!(script = %script.display(), "run: control-script installato per {user}");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        let (_user, home) = self.user_and_home(ctx)?;
        let (scripts_dir, script, localbin, symlink) = paths(&home);

        if ctx.dry_run {
            info!("undo (dry-run): rimuoverei script + symlink (e dir se create da noi)");
            return Ok(());
        }

        // Rimuovi solo ciò che abbiamo creato noi.
        if self.snap.symlink == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_symlink(&symlink) {
                warn!(error = %e, "undo: rimozione symlink fallita, proseguo (best-effort)");
            }
        }
        match (&self.snap.script, &self.snap.script_backup) {
            // Nostro: si rimuove.
            (PreState::CreatedByUs, _) => {
                if let Err(e) = self.ops.remove_file(&script) {
                    warn!(error = %e, "undo: rimozione script fallita, proseguo (best-effort)");
                }
            }
            // C'era già e l'abbiamo riscritto: torna quello di prima.
            (PreState::Preexisting, Some(backup)) => {
                let backup_path = std::path::Path::new(backup);
                if self.ops.path_exists(backup_path) {
                    if let Err(e) = self.ops.move_file(backup_path, &script) {
                        warn!(error = %e, "undo: ripristino del control-script fallito, proseguo (best-effort)");
                    } else {
                        info!("undo: control-script preesistente ripristinato dal backup");
                    }
                } else {
                    warn!(backup = %backup, "undo: backup del control-script non trovato");
                }
            }
            _ => {}
        }

        // Directory: rimuovi solo se create da noi e vuote.
        if !self.snap.scripts_dir_existed {
            remove_if_empty(self.ops.as_ref(), &scripts_dir);
        }
        if !self.snap.localbin_dir_existed {
            remove_if_empty(self.ops.as_ref(), &localbin);
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}

/// Rimuove una directory solo se vuota (best-effort).
fn remove_if_empty(ops: &dyn SystemOps, dir: &std::path::Path) {
    match ops.dir_is_empty(dir) {
        Ok(true) => {
            if let Err(e) = ops.rmdir(dir) {
                warn!(dir = %dir.display(), error = %e, "undo: rmdir fallito, proseguo (best-effort)");
            }
        }
        Ok(false) => info!(dir = %dir.display(), "undo: directory non vuota, la lascio"),
        Err(e) => warn!(dir = %dir.display(), error = %e, "undo: verifica directory fallita"),
    }
}
