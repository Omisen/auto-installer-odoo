//! [`PatchBashrc`] (14b): la mutazione chirurgica del file più personale
//! dell'utente — la **terza protezione critica** (C3).
//!
//! Appende `export PATH="$HOME/.local/bin:$PATH"` al `.bashrc` di `SUDO_USER`,
//! **solo se non c'è già** (match di riga esatto). Regole ferme di `CLAUDE.md`:
//! **mai riscrivere/troncare il file intero**, solo append/rimozione della
//! singola riga (o ripristino da backup).
//!
//! L'undo riporta il `.bashrc` **byte per byte** com'era: tutti gli alias e le
//! funzioni dell'utente intatti, senza la nostra riga, senza cicatrici. Il
//! metodo primario è il **ripristino da backup** (più robusto); il match-esatto
//! è il fallback.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::SystemOps;

/// La riga esatta che aggiungiamo (match `grep -Fqx`).
const PATH_LINE: &str = r#"export PATH="$HOME/.local/bin:$PATH""#;
const BASHRC_MODE: u32 = 0o644;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PatchBashrcSnapshot {
    /// `CreatedByUs` = riga aggiunta da noi; `Preexisting` = c'era già (no-op).
    pub prestate: PreState,
    /// Il `.bashrc` esisteva prima di noi?
    pub bashrc_existed: bool,
    /// Path del backup creato nel run (per il ripristino).
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

/// Rimuove **solo** le righe che corrispondono **esattamente** a `line` (mai un
/// match parziale/fuzzy: una riga simile scritta a mano dall'utente resta).
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

        // La nostra riga è già presente? (match esatto). Se sì → Preexisting:
        // non è nostra, non la tocchiamo.
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

        // Backup prima di modificare (se il file esisteva).
        if self.snap.bashrc_existed {
            let backup = format!("{}.bak.{}", bashrc.display(), unix_timestamp());
            self.ops.copy_file(&bashrc, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            info!(backup = %backup, "run: backup del .bashrc creato");
        }

        // Append della SINGOLA riga (mai riscrivere il file intero).
        self.ops.append_line(&bashrc, PATH_LINE)?;
        self.ops.chown_to_user(&bashrc, &user)?;

        self.snap.prestate = PreState::CreatedByUs;
        info!("run: riga PATH aggiunta al .bashrc di {user}");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // Agisce solo se la riga l'abbiamo aggiunta noi.
        if self.snap.prestate != PreState::CreatedByUs {
            info!("undo NO-OP: la riga PATH non è stata aggiunta da noi");
            return Ok(());
        }
        let (user, bashrc) = self.user_and_bashrc(ctx)?;
        if ctx.dry_run {
            info!("undo (dry-run): ripristino backup / rimozione riga esatta");
            return Ok(());
        }

        // Se il file non esisteva prima di noi, l'abbiamo creato: rimuovilo.
        if !self.snap.bashrc_existed {
            if let Err(e) = self.ops.remove_file(&bashrc) {
                warn!(error = %e, "undo: rimozione .bashrc creato da noi fallita, proseguo");
            }
            return Ok(());
        }

        // Metodo primario: ripristino da backup → file identico all'originale.
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

        // Fallback: rimuovi SOLO la riga esatta, senza toccare il resto.
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

    /// Terza protezione critica anche da disco. I tre campi contano tutti:
    /// `prestate` decide *se* toccare il file, `bashrc_existed` distingue
    /// "rimuovi la riga" da "rimuovi il file che abbiamo creato noi", e
    /// `backup_path` è ciò che rende il ripristino byte-per-byte. Un solo campo
    /// perso e l'undo o non agisce, o cancella un `.bashrc` che non era nostro.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
