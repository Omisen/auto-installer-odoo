//! [`NginxWriteConfig`] (13b): genera il vhost `sites-available/odoo<N>`.
//!
//! Come [`GenerateConfig`](crate::steps::generate_config): se il vhost
//! preesisteva, ne fa il **backup** e all'undo lo **ripristina**; se l'abbiamo
//! creato noi, all'undo lo rimuove. Gated da `--with-nginx`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

const VHOST_TEMPLATE: &str = include_str!("../../templates/nginx.conf.tpl");
const SITES_AVAILABLE: &str = "/etc/nginx/sites-available";
const VHOST_MODE: u32 = 0o644;
const DEFAULT_CLIENT_MAX: &str = "100m";
const DEFAULT_CERT_PATH: &str = "/etc/ssl/certs/odoo.crt";
const DEFAULT_KEY_PATH: &str = "/etc/ssl/private/odoo.key";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxWriteConfigSnapshot {
    pub prestate: PreState,
    pub backup_path: Option<String>,
}

pub struct NginxWriteConfig {
    ops: Box<dyn SystemOps>,
    snap: NginxWriteConfigSnapshot,
}

impl NginxWriteConfig {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxWriteConfigSnapshot::default(),
        }
    }

    fn dest(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{SITES_AVAILABLE}/odoo{}", ctx.odoo_version_short))
    }
    fn temp_path(dest: &std::path::Path) -> std::path::PathBuf {
        let parent = dest
            .parent()
            .unwrap_or_else(|| std::path::Path::new(SITES_AVAILABLE));
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "odoo".to_string());
        parent.join(format!(".{name}.tmp"))
    }
}

impl Default for NginxWriteConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for NginxWriteConfig {
    fn name(&self) -> &str {
        "nginx-write-config"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxWriteConfigSnapshot::default();
            return Ok(());
        }
        let dest = Self::dest(ctx);
        self.snap.prestate = if self.ops.path_exists(&dest) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(vhost = %dest.display(), prestate = ?self.snap.prestate, "snapshot nginx-write-config");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-write-config");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): renderizzerei e scriverei il vhost");
            return Ok(());
        }

        let dest = Self::dest(ctx);
        if self.snap.prestate == PreState::Preexisting {
            let backup = format!("{}.bak.{}", dest.display(), unix_timestamp());
            self.ops.copy_file(&dest, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            warn!(backup = %backup, "run: vhost esistente, backup creato");
        }

        let content = render_vhost(ctx);
        validate_vhost(&content)?;

        let tmp = Self::temp_path(&dest);
        self.ops.write_private_file(&tmp, &content)?;
        self.ops.move_file(&tmp, &dest)?;
        self.ops.chmod(&dest, VHOST_MODE)?;
        self.ops.chown_named(&dest, "root", "root")?;
        if self.snap.prestate == PreState::Untracked {
            self.snap.prestate = PreState::CreatedByUs;
        }
        info!(vhost = %dest.display(), "run: vhost nginx scritto");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rm o ripristino backup del vhost");
            return Ok(());
        }
        let dest = Self::dest(ctx);
        match self.snap.prestate {
            PreState::CreatedByUs => {
                if let Err(e) = self.ops.remove_file(&dest) {
                    warn!(error = %e, "undo: rimozione vhost fallita, proseguo (best-effort)");
                }
            }
            PreState::Preexisting => match &self.snap.backup_path {
                Some(backup) => {
                    let backup_path = std::path::Path::new(backup);
                    if self.ops.path_exists(backup_path) {
                        if let Err(e) = self.ops.move_file(backup_path, &dest) {
                            warn!(error = %e, "undo: ripristino backup vhost fallito, proseguo (best-effort)");
                        }
                    } else {
                        warn!(backup = %backup, "undo: backup vhost non trovato");
                    }
                }
                None => warn!("undo: nessun backup vhost registrato"),
            },
            PreState::Untracked => info!("undo NO-OP (vhost non generato da noi)"),
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Renderizza il vhost dal template embedded.
pub fn render_vhost(ctx: &Context) -> String {
    let port = ctx.port.to_string();
    let replacements: [(&str, &str); 5] = [
        ("{{NGINX_SERVER_NAME}}", ctx.nginx_server_name.as_str()),
        ("{{ODOO_PORT}}", port.as_str()),
        ("{{NGINX_CLIENT_MAX}}", DEFAULT_CLIENT_MAX),
        ("{{NGINX_CERT_PATH}}", DEFAULT_CERT_PATH),
        ("{{NGINX_KEY_PATH}}", DEFAULT_KEY_PATH),
    ];
    let mut out = VHOST_TEMPLATE.to_string();
    for (token, value) in replacements {
        out = out.replace(token, value);
    }
    out
}

/// Nessun placeholder `{{...}}` residuo nel vhost.
pub fn validate_vhost(content: &str) -> Result<(), StepError> {
    if content.contains("{{") {
        return Err(StepError::Precondition(
            "vhost nginx: placeholder {{...}} non sostituiti".to_string(),
        ));
    }
    Ok(())
}
