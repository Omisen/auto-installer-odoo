//! [`NginxWriteConfig`]: renders the Odoo vhost.
//!
//! like [`GenerateConfig`](crate::steps::generate_config): a pre-existing vhost
//! is **backed up** and **restored** by the undo, while one of ours is removed.
//! gated on `--with-nginx`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VHOST_TEMPLATE: &str = include_str!("../../templates/nginx.conf.tpl");
const VHOST_MODE: u32 = 0o644;
const DEFAULT_CLIENT_MAX: &str = "100m";

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
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxWriteConfigSnapshot::default(),
        }
    }

    /// where the vhost goes, per this family's conventions.
    ///
    /// not a constant: on one family the directory differs and the file must
    /// carry an extension, or nginx will not load it — and would not say so.
    fn dest(&self, ctx: &Context) -> std::path::PathBuf {
        self.ops
            .distro()
            .nginx_layout()
            .vhost_path(&ctx.artifact_base())
    }
    /// a private temporary beside the destination, so the move is an atomic
    /// rename; unpredictable name, fail-closed creation.
    fn temp_path(dest: &std::path::Path) -> std::path::PathBuf {
        crate::system_ops::private_temp_path(dest, "odoo")
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
        let dest = self.dest(ctx);
        self.snap.prestate = if self.ops.path_exists(&dest) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(vhost = %dest.display(), prestate = ?self.snap.prestate, "snapshot: nginx-write-config");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx not requested, skipping nginx-write-config");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry run): would render and write the vhost");
            return Ok(());
        }

        let dest = self.dest(ctx);
        if self.snap.prestate == PreState::Preexisting {
            let backup = format!("{}.bak.{}", dest.display(), unix_timestamp());
            self.ops.copy_file(&dest, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            warn!(backup = %backup, "run: the vhost existed, a backup was made");
        }

        let content = render_vhost(ctx);
        validate_vhost(&content)?;

        let tmp = Self::temp_path(&dest);
        self.ops.create_private_file(&tmp, &content)?;
        // a randomly named temporary in a system directory the rollback does
        // not clean: if the move fails, we remove it ourselves.
        if let Err(e) = self.ops.move_file(&tmp, &dest) {
            let _ = self.ops.remove_file(&tmp);
            return Err(e);
        }
        self.ops.chmod(&dest, VHOST_MODE)?;
        self.ops.chown_named(&dest, "root", "root")?;
        if self.snap.prestate == PreState::Untracked {
            self.snap.prestate = PreState::CreatedByUs;
        }
        info!(vhost = %dest.display(), "run: nginx vhost written");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry run): remove the vhost, or restore its backup");
            return Ok(());
        }
        let dest = self.dest(ctx);
        match self.snap.prestate {
            PreState::CreatedByUs => {
                if let Err(e) = self.ops.remove_file(&dest) {
                    warn!(error = %e, "undo: removing the vhost failed, proceeding (best-effort)");
                }
            }
            PreState::Preexisting => match &self.snap.backup_path {
                Some(backup) => {
                    let backup_path = std::path::Path::new(backup);
                    if self.ops.path_exists(backup_path) {
                        if let Err(e) = self.ops.move_file(backup_path, &dest) {
                            warn!(error = %e, "undo: restoring the vhost backup failed, proceeding (best-effort)");
                        }
                    } else {
                        warn!(backup = %backup, "undo: the vhost backup was not found");
                    }
                }
                None => warn!("undo: no vhost backup recorded"),
            },
            PreState::Untracked => info!("undo NO-OP (vhost not generated by us)"),
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

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// renders the vhost from the embedded template.
pub fn render_vhost(ctx: &Context) -> String {
    let port = ctx.port.to_string();
    // no certificate placeholders: the vhost has no 443 block, and the two that
    // existed were substituted inside commented-out lines, suggesting TLS was
    // configured here (A-V3-6).
    let gevent_port = ctx.gevent_port.to_string();
    let base = ctx.artifact_base();
    let replacements: [(&str, &str); 5] = [
        ("{{NGINX_SERVER_NAME}}", ctx.nginx_server_name.as_str()),
        ("{{ODOO_PORT}}", port.as_str()),
        ("{{ODOO_GEVENT_PORT}}", gevent_port.as_str()),
        ("{{NGINX_CLIENT_MAX}}", DEFAULT_CLIENT_MAX),
        ("{{INSTANCE_BASE}}", base.as_str()),
    ];
    let mut out = VHOST_TEMPLATE.to_string();
    for (token, value) in replacements {
        out = out.replace(token, value);
    }
    out
}

/// checks that no placeholder is left in the vhost.
pub fn validate_vhost(content: &str) -> Result<(), StepError> {
    if content.contains("{{") {
        return Err(StepError::Precondition(
            "nginx vhost: unsubstituted {{...}} placeholders".to_string(),
        ));
    }
    Ok(())
}
