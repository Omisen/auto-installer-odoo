//! [`NginxReload`]: validates and reloads Nginx.
//!
//! `run` runs `nginx -t` and reloads **only if the config is valid**. it has no
//! artifacts of its own: the real restoration happens in the other sub-steps,
//! and here the undo stops Nginx if we started it (D4).
//!
//! if Nginx was the customer's, the undo leaves it running **without
//! reloading**: being the first undo of the phase, the configurations are not
//! restored yet. the realignment reload lives at the end of the phase, in
//! [`NginxInstall`](crate::steps::nginx_install).

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const NGINX_SERVICE: &str = "nginx";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxReloadSnapshot {
    /// was Nginx already running before us?
    pub active: PreState,
}

pub struct NginxReload {
    ops: Box<dyn SystemOps>,
    snap: NginxReloadSnapshot,
}

impl NginxReload {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxReloadSnapshot::default(),
        }
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
            info!("nginx not requested, skipping nginx-reload");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry run): nginx -t, then reload or start");
            return Ok(());
        }

        // never reload a config that fails `nginx -t`.
        if !self.ops.nginx_test() {
            return Err(StepError::Precondition(
                "nginx -t reported errors: an invalid config is not reloaded".to_string(),
            ));
        }

        if self.ops.service_is_active(NGINX_SERVICE) {
            self.ops.service_reload(NGINX_SERVICE)?;
        } else {
            self.ops.service_start(NGINX_SERVICE)?;
            self.snap.active = PreState::CreatedByUs;
        }
        info!("run: nginx reloaded and active");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry run): stop nginx if we started it, otherwise reload");
            return Ok(());
        }
        match self.snap.active {
            // stop only what we started (D4).
            PreState::CreatedByUs => {
                if let Err(e) = self.ops.service_stop(NGINX_SERVICE) {
                    warn!(error = %e, "undo: stopping nginx failed, proceeding (best-effort)");
                }
            }
            // already running before us, so it stays running (D4) — but **no
            // reload here**. this is the *first* undo of the phase, and the
            // configurations are not restored yet: reloading now would load
            // exactly the state we are about to dismantle, and nothing would
            // reload afterwards.
            //
            // the realignment happens at the **end** of the phase, in
            // `NginxInstall::undo`.
            PreState::Preexisting => {
                info!(
                    "undo: nginx was already active, leaving it so (the final reload is in nginx-install)"
                );
            }
            // gated off, or nothing done: a no-op.
            PreState::Untracked => {}
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
