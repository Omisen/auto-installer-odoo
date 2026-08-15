//! [`NginxInstall`]: installs and enables Nginx, under `--with-nginx`.
//!
//! the first of the six sub-steps `setup_nginx` is split into. all of them are
//! **gated** on `ctx.with_nginx`: without it the whole phase is inert, exactly
//! like `SetupLogDir` with no log file.
//!
//! policy consistent with PostgreSQL (D3): the undo does **stop + disable** but
//! does **not** purge by default.
//!
//! being the **first** sub-step it is the **last** to be undone, so its undo
//! hosts the final realignment reload for when Nginx survives the rollback
//! because it was the customer's. see
//! [`NginxReload`](crate::steps::nginx_reload).

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const NGINX_SERVICE: &str = "nginx";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxInstallSnapshot {
    pub installed: PreState,
    pub enabled: PreState,
}

pub struct NginxInstall {
    ops: Box<dyn SystemOps>,
    snap: NginxInstallSnapshot,
}

impl NginxInstall {
    /// the nginx package, per this family's manager.
    fn package(&self) -> String {
        self.ops.packages().catalog().nginx
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxInstallSnapshot::default(),
        }
    }
}

impl Step for NginxInstall {
    fn name(&self) -> &str {
        "nginx-install"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxInstallSnapshot::default(); // tutto Untracked
            return Ok(());
        }
        self.snap.installed = if self.ops.packages().is_installed(&self.package()) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.enabled = if self.ops.service_is_enabled(NGINX_SERVICE) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(installed = ?self.snap.installed, enabled = ?self.snap.enabled, "snapshot: nginx-install");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx not requested, skipping nginx-install");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry run): would install and enable nginx");
            return Ok(());
        }
        if self.snap.installed == PreState::Untracked {
            self.ops.packages().install(&[self.package().as_str()])?;
            self.snap.installed = PreState::CreatedByUs;
        }
        if self.snap.enabled == PreState::Untracked {
            self.ops.service_enable(NGINX_SERVICE)?;
            self.snap.enabled = PreState::CreatedByUs;
        }
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry run): stop and disable nginx; purge only if aggressive");
            return Ok(());
        }
        // nginx belongs to every instance that proxies through it, so with
        // another one installed it is neither stopped nor purged (phase I2).
        // the reload below still happens: our vhost is gone either way, and a
        // running nginx would go on serving the config it loaded (A1.4).
        let shared_in_use = ctx.shared_in_use();
        if shared_in_use {
            info!(
                "undo: nginx left running and installed — another instance is still \
                 configured behind it"
            );
        }

        // stop and disable only what we enabled (D4, D3).
        if !shared_in_use && self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(NGINX_SERVICE) {
                warn!(error = %e, "undo: stopping nginx failed, proceeding (best-effort)");
            }
            if let Err(e) = self.ops.service_disable(NGINX_SERVICE) {
                warn!(error = %e, "undo: disabling nginx failed, proceeding (best-effort)");
            }
        }
        // purge only under `--aggressive-rollback` (D3).
        if !shared_in_use && self.snap.installed == PreState::CreatedByUs {
            if ctx.aggressive_rollback {
                crate::steps::remove_with_recovery(
                    self.ops.packages(),
                    "nginx-install",
                    &[self.package().as_str()],
                );
                if let Err(e) = self.ops.packages().remove_orphans() {
                    warn!(error = %e, "undo: autoremove failed, proceeding (best-effort)");
                }
            } else {
                info!("undo: nginx left installed (stop and disable are reversible; use --aggressive-rollback to purge)");
            }
        }

        // the final realignment. this is the **last** nginx sub-step to be
        // undone, so the files are already restored: our vhost is gone and the
        // customer's default site is back. if nginx is still up — theirs, not
        // ours — it is nonetheless still **serving** the config it loaded,
        // ours. without this reload the files would be right while their site
        // stayed down until someone reloaded by hand.
        //
        // as in the run: never reload a config that fails `nginx -t`.
        if self.ops.service_is_active(NGINX_SERVICE) {
            if !self.ops.nginx_test() {
                warn!(
                    "undo: the nginx config is invalid after the restore, not reloading \
                     (check `nginx -t`)"
                );
            } else if let Err(e) = self.ops.service_reload(NGINX_SERVICE) {
                warn!(error = %e, "undo: the final nginx reload failed, proceeding (best-effort)");
            } else {
                info!("undo: nginx reloaded with the restored config");
            }
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
