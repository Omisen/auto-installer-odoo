//! [`NginxInstall`] (13a): installa e abilita Nginx, se `--with-nginx`.
//!
//! Primo dei cinque sotto-step in cui spezziamo `setup_nginx`. Tutti sono
//! **gated** da `ctx.with_nginx`: se assente, l'intera fase è inerte (snapshot
//! `Untracked`, run/undo no-op), esattamente come `SetupLogDir` col logfile
//! disabilitato.
//!
//! Coerenza di policy con PostgreSQL (D3): l'undo fa **stop + disable** ma
//! **NON purga** di default; il purge solo con `--aggressive-rollback`.
//!
//! Essendo il **primo** sotto-step della fase, è l'**ultimo** a essere annullato:
//! il suo undo ospita perciò il reload di riallineamento finale, quando Nginx
//! sopravvive al rollback perché era del cliente. Vedi
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
    /// Il pacchetto di nginx secondo il gestore di questa famiglia.
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
        info!(installed = ?self.snap.installed, enabled = ?self.snap.enabled, "snapshot nginx-install");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-install");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): installerei/abiliterei nginx");
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
            info!("undo (dry-run): stop/disable nginx; purge solo se aggressive");
            return Ok(());
        }
        // stop + disable se abilitato da noi (D4/D3).
        if self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(NGINX_SERVICE) {
                warn!(error = %e, "undo: stop nginx fallito, proseguo (best-effort)");
            }
            if let Err(e) = self.ops.service_disable(NGINX_SERVICE) {
                warn!(error = %e, "undo: disable nginx fallito, proseguo (best-effort)");
            }
        }
        // purge solo con --aggressive-rollback (coerenza D3).
        if self.snap.installed == PreState::CreatedByUs {
            if ctx.aggressive_rollback {
                crate::steps::remove_with_recovery(
                    self.ops.packages(),
                    "nginx-install",
                    &[self.package().as_str()],
                );
                if let Err(e) = self.ops.packages().remove_orphans() {
                    warn!(error = %e, "undo: autoremove fallito, proseguo (best-effort)");
                }
            } else {
                info!("undo: nginx lasciato installato (stop+disable reversibili; usa --aggressive-rollback per purgare)");
            }
        }

        // Riallineamento finale. Questo è l'**ultimo** sotto-step Nginx a essere
        // annullato (gli undo girano in ordine inverso), quindi qui i file sono
        // già stati ripristinati: il nostro vhost è rimosso e il default site
        // del cliente è tornato al suo posto. Se nginx è ancora in piedi — cioè
        // era suo, non nostro — sta però ancora **servendo** la config che
        // aveva caricato, la nostra. Senza questo reload i file sarebbero a
        // posto ma il sito del cliente resterebbe giù fino a un reload manuale.
        //
        // Come nel run: non si ricarica una config che non passa `nginx -t`.
        if self.ops.service_is_active(NGINX_SERVICE) {
            if !self.ops.nginx_test() {
                warn!(
                    "undo: config nginx non valida dopo il ripristino, non ricarico \
                     (controlla `nginx -t`)"
                );
            } else if let Err(e) = self.ops.service_reload(NGINX_SERVICE) {
                warn!(error = %e, "undo: reload finale nginx fallito, proseguo (best-effort)");
            } else {
                info!("undo: nginx ricaricato con la config ripristinata");
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
