//! [`NginxReload`] (13e): valida e ricarica Nginx.
//!
//! `run` esegue `nginx -t` e ricarica **solo se la config è valida** (non si
//! ricarica una config rotta). Non ha artefatti propri da rimuovere: il "vero"
//! ripristino avviene tramite gli altri sotto-step (vhost/site rimossi o
//! ripristinati); qui l'undo ferma Nginx se l'abbiamo avviato noi (D4), oppure
//! tenta un reload best-effort per riallineare.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

const NGINX_SERVICE: &str = "nginx";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxReloadSnapshot {
    /// Nginx era già attivo prima di noi?
    pub active: PreState,
}

pub struct NginxReload {
    ops: Box<dyn SystemOps>,
    snap: NginxReloadSnapshot,
}

impl NginxReload {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxReloadSnapshot::default(),
        }
    }
}

impl Default for NginxReload {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for NginxReload {
    fn name(&self) -> &str {
        "nginx-reload"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxReloadSnapshot::default();
            return Ok(());
        }
        self.snap.active = if self.ops.service_is_active(NGINX_SERVICE) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-reload");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): nginx -t poi reload/start");
            return Ok(());
        }

        // Non ricaricare una config che non passa nginx -t.
        if !self.ops.nginx_test() {
            return Err(StepError::Precondition(
                "nginx -t ha riportato errori: non ricarico una config non valida".to_string(),
            ));
        }

        if self.ops.service_is_active(NGINX_SERVICE) {
            self.ops.service_reload(NGINX_SERVICE)?;
        } else {
            self.ops.service_start(NGINX_SERVICE)?;
            self.snap.active = PreState::CreatedByUs;
        }
        info!("run: nginx ricaricato/attivo");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): stop nginx se avviato da noi, altrimenti reload");
            return Ok(());
        }
        match self.snap.active {
            // Se l'avevamo avviato noi (era fermo prima) → stop (D4).
            PreState::CreatedByUs => {
                if let Err(e) = self.ops.service_stop(NGINX_SERVICE) {
                    warn!(error = %e, "undo: stop nginx fallito, proseguo (best-effort)");
                }
            }
            // Nginx era già attivo prima di noi: reload best-effort per
            // riallineare. Il ripristino delle config avviene negli altri
            // sotto-step (vhost/site rimossi o ripristinati).
            PreState::Preexisting => {
                if let Err(e) = self.ops.service_reload(NGINX_SERVICE) {
                    warn!(error = %e, "undo: reload nginx fallito, proseguo (best-effort)");
                }
            }
            // Untracked: fase gated off o nulla fatto → no-op.
            PreState::Untracked => {}
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }
}
