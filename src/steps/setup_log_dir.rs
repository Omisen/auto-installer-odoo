//! [`SetupLogDir`]: crea la directory del logfile, **solo se** `ODOO_LOGFILE` è
//! configurato.
//!
//! Default di progetto: nessun logfile → Odoo logga su journal/stdout, quindi
//! non c'è alcuna directory da creare e lo step è interamente `Untracked`/no-op.
//!
//! Gira **dopo** [`CreateOdooUser`](crate::steps::create_odoo_user), quindi
//! l'utente `odoo` esiste già e la log dir può essere chownata a lui.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{RealSystemOps, SystemOps};

/// Permessi della log dir (owner rwx, group r-x, other ---).
const LOG_DIR_MODE: u32 = 0o750;

/// Crea la directory del logfile (reversibile) se `ODOO_LOGFILE` è impostato.
pub struct SetupLogDir {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl SetupLogDir {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    /// Costruttore con `SystemOps` iniettabile (usato dai test con un mock).
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }
}

impl Default for SetupLogDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for SetupLogDir {
    fn name(&self) -> &str {
        "setup-log-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let Some(logfile) = &ctx.odoo_logfile else {
            self.prestate = PreState::Untracked;
            info!("snapshot: ODOO_LOGFILE disabilitato, step no-op");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            self.prestate = PreState::Untracked;
            warn!(logfile = %logfile.display(), "snapshot: logfile senza directory genitrice, step no-op");
            return Ok(());
        };

        self.prestate = if self.ops.path_exists(dir) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(dir = %dir.display(), prestate = ?self.prestate, "snapshot setup-log-dir");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let Some(logfile) = &ctx.odoo_logfile else {
            info!("run: ODOO_LOGFILE disabilitato, nessuna azione");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            return Ok(());
        };
        let user = &ctx.odoo_user;

        // La dir esisteva già: non è nostra, non la tocchiamo (né owner né log).
        if self.prestate == PreState::Preexisting {
            info!(dir = %dir.display(), "run: log dir già presente, skip");
            return Ok(());
        }

        if ctx.dry_run {
            info!(dir = %dir.display(), "run (dry-run): creerei la log dir + chown {user}:{user} 0750");
            return Ok(());
        }

        self.ops.mkdir(dir)?;
        self.ops.chown_named(dir, user, user)?;
        self.ops.chmod(dir, LOG_DIR_MODE)?;

        self.prestate = PreState::CreatedByUs;
        info!(dir = %dir.display(), "run: log dir creata, owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (log dir non creata da noi)");
            return Ok(());
        }

        // Se siamo CreatedByUs, il logfile e la sua dir sono sicuramente definiti.
        let dir = match ctx.odoo_logfile.as_ref().and_then(|f| f.parent()) {
            Some(dir) => dir,
            None => return Ok(()),
        };

        if ctx.dry_run {
            info!(dir = %dir.display(), "undo (dry-run): rimuoverei la log dir se vuota");
            return Ok(());
        }

        // Rimuovi solo se vuota: mai cancellare log già scritti; niente rm -rf.
        match self.ops.dir_is_empty(dir) {
            Ok(true) => match self.ops.rmdir(dir) {
                Ok(()) => info!(dir = %dir.display(), "undo: log dir rimossa"),
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "undo: rmdir fallito, proseguo (best-effort)")
                }
            },
            Ok(false) => {
                warn!(dir = %dir.display(), "undo: log dir non vuota, non la rimuovo (best-effort)")
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: impossibile verificare la log dir, non rimuovo")
            }
        }
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
