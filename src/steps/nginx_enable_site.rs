//! [`NginxEnableSite`] (13c): abilita il sito e libera la porta 80.
//!
//! # Mutazione insidiosa: il default site
//!
//! Per liberare la porta 80, il Bash rimuove `sites-enabled/default` — un
//! symlink che è **config preesistente del cliente**. Qui lo snapshot torna a
//! essere **protezione**: registra se il default site esisteva prima di noi, e
//! l'undo lo **ripristina**. È l'analogo del backup di
//! [`GenerateConfig`](crate::steps::generate_config), applicato a un symlink: la
//! promessa "chirurgico" estesa alla config Nginx di terzi.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{RealSystemOps, SystemOps};

const SITES_AVAILABLE: &str = "/etc/nginx/sites-available";
const SITES_ENABLED: &str = "/etc/nginx/sites-enabled";
/// Target standard Debian/Ubuntu del default site (per il ripristino).
const DEFAULT_SITE_TARGET: &str = "/etc/nginx/sites-available/default";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxEnableSiteSnapshot {
    /// Stato del nostro symlink `sites-enabled/odoo<N>`.
    pub link: PreState,
    /// Il default site esisteva prima di noi? (per ripristinarlo all'undo).
    pub default_site_existed: bool,
}

pub struct NginxEnableSite {
    ops: Box<dyn SystemOps>,
    snap: NginxEnableSiteSnapshot,
}

impl NginxEnableSite {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxEnableSiteSnapshot::default(),
        }
    }

    fn src(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_AVAILABLE}/odoo{}", ctx.odoo_version_short))
    }
    fn link(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_ENABLED}/odoo{}", ctx.odoo_version_short))
    }
    fn default_site() -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_ENABLED}/default"))
    }
}

impl Default for NginxEnableSite {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for NginxEnableSite {
    fn name(&self) -> &str {
        "nginx-enable-site"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxEnableSiteSnapshot::default();
            return Ok(());
        }
        self.snap.link = if self.ops.symlink_exists(&Self::link(ctx)) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        // Informazione chiave per l'undo: il default site c'era?
        self.snap.default_site_existed = self.ops.symlink_exists(&Self::default_site());
        info!(
            link = ?self.snap.link,
            default_site_existed = self.snap.default_site_existed,
            "snapshot nginx-enable-site"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-enable-site");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): rimuoverei il default site e creerei il symlink");
            return Ok(());
        }

        // Libera la porta 80 rimuovendo il default site (ne abbiamo registrato
        // l'esistenza nello snapshot per poterlo ripristinare).
        if self.snap.default_site_existed {
            if let Err(e) = self.ops.remove_symlink(&Self::default_site()) {
                warn!(error = %e, "run: rimozione default site fallita, proseguo");
            } else {
                info!("run: default site nginx rimosso (porta 80 liberata)");
            }
        }

        self.ops.create_symlink(&Self::src(ctx), &Self::link(ctx))?;
        if self.snap.link == PreState::Untracked {
            self.snap.link = PreState::CreatedByUs;
        }
        info!("run: sito abilitato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rm nostro symlink + ripristino default site se esisteva");
            return Ok(());
        }

        // Rimuovi il nostro symlink se creato da noi.
        if self.snap.link == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_symlink(&Self::link(ctx)) {
                warn!(error = %e, "undo: rimozione symlink nostro fallita, proseguo (best-effort)");
            }
        }

        // Ripristina il default site se esisteva prima di noi: riporta la config
        // Nginx del cliente com'era.
        if self.snap.default_site_existed {
            let target = std::path::PathBuf::from(DEFAULT_SITE_TARGET);
            if let Err(e) = self.ops.create_symlink(&target, &Self::default_site()) {
                warn!(error = %e, "undo: ripristino default site fallito, proseguo (best-effort)");
            } else {
                info!("undo: default site nginx ripristinato");
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// `default_site_existed` è l'informazione che permette all'undo di
    /// **ricucire** la config del cliente: senza reidratarla, un rollback da
    /// disco rimuoverebbe il nostro vhost e lascerebbe la porta 80 senza il
    /// default site che avevamo tolto.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
