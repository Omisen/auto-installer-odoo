//! [`PatchBashrc`]: the surgical mutation of the user's most personal file —
//! the **third critical protection** (C3).
//!
//! appends one `PATH` line to `SUDO_USER`'s `.bashrc`, **only if it is not
//! already there**, matched exactly. firm rules from `CLAUDE.md`: **never
//! rewrite or truncate the whole file**, only append or remove that single
//! line, or restore the backup.
//!
//! the undo brings the file back **byte for byte**: every alias and function
//! intact, without our line and without scars. restoring from the backup is the
//! primary method; the exact match is the fallback.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::SystemOps;

/// the exact line we add.
const PATH_LINE: &str = r#"export PATH="$HOME/.local/bin:$PATH""#;
const BASHRC_MODE: u32 = 0o644;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PatchBashrcSnapshot {
    /// whether we added the line, or it was already there.
    pub prestate: PreState,
    /// did the `.bashrc` exist before us?
    pub bashrc_existed: bool,
    /// path of the backup taken during the run.
    pub backup_path: Option<String>,
}

pub struct PatchBashrc {
    ops: Box<dyn SystemOps>,
    snap: PatchBashrcSnapshot,
}

impl PatchBashrc {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: PatchBashrcSnapshot::default(),
        }
    }

    fn user_and_bashrc(&self, ctx: &Context) -> Result<(String, std::path::PathBuf), StepError> {
        let user = ctx.sudo_user.clone().ok_or_else(|| {
            StepError::Precondition(
                "SUDO_USER non disponibile: l'installer deve girare via sudo".to_string(),
            )
        })?;
        let home = self.ops.getent_home(&user)?.ok_or_else(|| {
            StepError::Precondition(format!("impossibile determinare la home per '{user}'"))
        })?;
        Ok((user, std::path::PathBuf::from(home).join(".bashrc")))
    }
}

/// removes **only** the lines matching `line` **exactly**: never a fuzzy match,
/// so a similar handwritten line of the user's survives.
pub fn remove_exact_line(content: &str, line: &str) -> String {
    let kept: Vec<&str> = content.lines().filter(|l| *l != line).collect();
    let mut out = kept.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

impl Step for PatchBashrc {
    fn name(&self) -> &str {
        "patch-bashrc"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let (_user, bashrc) = self.user_and_bashrc(ctx)?;
        self.snap.bashrc_existed = self.ops.path_exists(&bashrc);

        // our line already there means it is not ours to touch.
        let line_present = if self.snap.bashrc_existed {
            let content = self.ops.read_to_string(&bashrc)?;
            content.lines().any(|l| l == PATH_LINE)
        } else {
            false
        };
        self.snap.prestate = if line_present {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(bashrc_existed = self.snap.bashrc_existed, prestate = ?self.snap.prestate, "snapshot patch-bashrc");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate == PreState::Preexisting {
            info!("run: riga PATH già presente, nessuna modifica");
            return Ok(());
        }
        let (user, bashrc) = self.user_and_bashrc(ctx)?;
        if ctx.dry_run {
            info!("run (dry-run): appenderei la riga PATH al .bashrc di {user}");
            return Ok(());
        }

        // back up before modifying, when the file existed.
        if self.snap.bashrc_existed {
            let backup = format!("{}.bak.{}", bashrc.display(), unix_timestamp());
            self.ops.copy_file(&bashrc, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            info!(backup = %backup, "run: backup del .bashrc creato");
        }

        // append the SINGLE line; never rewrite the whole file.
        self.ops.append_line(&bashrc, PATH_LINE)?;
        self.ops.chown_to_user(&bashrc, &user)?;

        self.snap.prestate = PreState::CreatedByUs;
        info!("run: riga PATH aggiunta al .bashrc di {user}");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // acts only on a line we added.
        if self.snap.prestate != PreState::CreatedByUs {
            info!("undo NO-OP: la riga PATH non è stata aggiunta da noi");
            return Ok(());
        }
        let (user, bashrc) = self.user_and_bashrc(ctx)?;
        if ctx.dry_run {
            info!("undo (dry-run): ripristino backup / rimozione riga esatta");
            return Ok(());
        }

        // a file that did not exist before us is ours to remove.
        if !self.snap.bashrc_existed {
            if let Err(e) = self.ops.remove_file(&bashrc) {
                warn!(error = %e, "undo: rimozione .bashrc creato da noi fallita, proseguo");
            }
            return Ok(());
        }

        // primary method: restore the backup, giving an identical file.
        if let Some(backup) = &self.snap.backup_path {
            let backup_path = std::path::Path::new(backup);
            if self.ops.path_exists(backup_path) {
                if let Err(e) = self.ops.move_file(backup_path, &bashrc) {
                    warn!(error = %e, "undo: ripristino backup fallito, provo il fallback match-esatto");
                } else {
                    info!("undo: .bashrc ripristinato dal backup (identico all'originale)");
                    return Ok(());
                }
            } else {
                warn!(backup = %backup, "undo: backup non trovato, uso il fallback match-esatto");
            }
        }

        // fallback: remove ONLY the exact line, touching nothing else.
        match self.ops.read_to_string(&bashrc) {
            Ok(content) => {
                let cleaned = remove_exact_line(&content, PATH_LINE);
                self.ops.write_private_file(&bashrc, &cleaned)?;
                let _ = self.ops.chmod(&bashrc, BASHRC_MODE);
                let _ = self.ops.chown_to_user(&bashrc, &user);
                info!("undo: rimossa la sola riga PATH aggiunta da noi");
            }
            Err(e) => warn!(error = %e, "undo: impossibile leggere il .bashrc, non modifico"),
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// the third critical protection, from disk too. all three fields matter:
    /// the `PreState` decides *whether* to touch the file, `bashrc_existed`
    /// tells "remove the line" from "remove the file we created", and
    /// `backup_path` is what makes the restore byte-for-byte. lose one and the
    /// undo either does nothing or deletes a `.bashrc` that was not ours.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
