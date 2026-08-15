//! [`SetupSystemd`]: installs and activates Odoo's systemd service.
//!
//! reuses two proven patterns: the **three independent `PreState`s** of
//! [`SetupPostgres`](crate::steps::setup_postgres) under the same D4 rule, and
//! the **embedded template** rendering of
//! [`GenerateConfig`](crate::steps::generate_config). no data protection and no
//! shared state — the three axes are local to the step.
//!
//! the specific care is the **undo's order**, stop → disable → rm → reload, so
//! systemd is never left referencing a unit that has vanished.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// the unit template, embedded in the binary.
const SERVICE_TEMPLATE: &str = include_str!("../../templates/odoo.service.tpl");
const UNIT_MODE: u32 = 0o644;
const REPO_SUBDIR: &str = "odoo";
const VENV_SUBDIR: &str = "sandbox";
/// how long production waits for the service to settle after starting.
const DEFAULT_SETTLE_SECS: u64 = 3;

/// snapshot of the service's three independent axes.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemdSnapshot {
    pub unit_file: PreState,
    pub enabled: PreState,
    pub active: PreState,
}

/// installs, enables and starts the service, restoring each axis on undo.
pub struct SetupSystemd {
    ops: Box<dyn SystemOps>,
    settle_secs: u64,
    snap: SystemdSnapshot,
}

impl SetupSystemd {
    /// the **production** constructor: waits for the service to settle before
    /// checking its state.
    ///
    /// [`Self::with_ops`] zeroes that wait because tests must not sleep, and
    /// using it for a real `run` would query `is-active` at the very instant of
    /// the start, when a real service is not up yet.
    pub fn for_run(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            settle_secs: DEFAULT_SETTLE_SECS,
            snap: SystemdSnapshot::default(),
        }
    }

    /// the test constructor: injectable `SystemOps`, no waiting.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            settle_secs: 0,
            snap: SystemdSnapshot::default(),
        }
    }

    fn unit_name(ctx: &Context) -> String {
        ctx.artifact_base()
    }

    fn unit_path(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/etc/systemd/system/{}.service",
            Self::unit_name(ctx)
        ))
    }

    /// a private temporary beside the unit, so the move is an atomic rename;
    /// unpredictable name, fail-closed creation.
    fn temp_path(unit_path: &std::path::Path) -> std::path::PathBuf {
        crate::system_ops::private_temp_path(unit_path, "odoo.service")
    }

    fn settle(&self) {
        if self.settle_secs > 0 {
            std::thread::sleep(std::time::Duration::from_secs(self.settle_secs));
        }
    }
}

impl Step for SetupSystemd {
    fn name(&self) -> &str {
        "setup-systemd"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let unit_path = Self::unit_path(ctx);
        let unit = Self::unit_name(ctx);

        self.snap.unit_file = if self.ops.path_exists(&unit_path) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.enabled = if self.ops.service_is_enabled(&unit) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        self.snap.active = if self.ops.service_is_active(&unit) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(
            unit_file = ?self.snap.unit_file,
            enabled = ?self.snap.enabled,
            active = ?self.snap.active,
            "snapshot: setup-systemd"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry run): would render the unit, install it, then enable and start");
            return Ok(());
        }

        let unit = Self::unit_name(ctx);
        let unit_path = Self::unit_path(ctx);

        // render and validate the unit.
        let content = render_unit(ctx);
        validate_unit(&content)?;

        // idempotent: overwrites. the `PreState` only governs the undo.
        let tmp = Self::temp_path(&unit_path);
        self.ops.create_private_file(&tmp, &content)?;
        // a randomly named temporary in a directory the rollback does not
        // clean: if the move fails, we remove it ourselves.
        if let Err(e) = self.ops.move_file(&tmp, &unit_path) {
            let _ = self.ops.remove_file(&tmp);
            return Err(e);
        }
        // the unit is in place: ours from here (A-V3-24), so a failure in the
        // ownership calls below does not leave systemd holding our file.
        if self.snap.unit_file == PreState::Untracked {
            self.snap.unit_file = PreState::CreatedByUs;
        }
        self.ops.chmod(&unit_path, UNIT_MODE)?;
        self.ops.chown_named(&unit_path, "root", "root")?;
        self.ops.daemon_reload()?;

        // enable only if it was not already enabled (D4).
        if self.snap.enabled == PreState::Untracked {
            self.ops.service_enable(&unit)?;
            self.snap.enabled = PreState::CreatedByUs;
        }

        // start, or restart if already running, to apply the new config.
        if self.snap.active == PreState::Preexisting {
            self.ops.service_restart(&unit)?;
        } else {
            self.ops.service_start(&unit)?;
            self.snap.active = PreState::CreatedByUs;
        }

        // settle, then verify.
        self.settle();
        if !self.ops.service_is_active(&unit) {
            return Err(StepError::Precondition(format!(
                "service '{unit}' is not active after the start. \
                 check: journalctl -u {unit} -n 50 --no-pager"
            )));
        }
        info!(unit = %unit, "run: systemd service installed and active");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!(
                "undo (dry run): stop, disable, remove and daemon-reload according to the snapshot"
            );
            return Ok(());
        }

        let unit = Self::unit_name(ctx);
        let unit_path = Self::unit_path(ctx);

        // the order is crucial: stop → disable → rm → reload.

        // stop only what we started (D4).
        if self.snap.active == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(&unit) {
                warn!(error = %e, "undo: stopping the service failed, proceeding (best-effort)");
            }
        }
        // disable only what we enabled (D4).
        if self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_disable(&unit) {
                warn!(error = %e, "undo: disabling the service failed, proceeding (best-effort)");
            }
        }
        // remove the unit file only if we created it.
        if self.snap.unit_file == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_file(&unit_path) {
                warn!(error = %e, "undo: removing the unit file failed, proceeding (best-effort)");
            }
        }
        // the final reload makes systemd forget the removed unit.
        if let Err(e) = self.ops.daemon_reload() {
            warn!(error = %e, "undo: daemon-reload failed, proceeding (best-effort)");
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

/// renders the unit from the embedded template.
pub fn render_unit(ctx: &Context) -> String {
    let install = ctx.install_dir.to_string_lossy();
    let base = ctx.artifact_base();
    let qualified = ctx.qualified_name();
    // the finished path, from the one function that decides it — the template
    // no longer composes it out of parts.
    let conf = crate::steps::generate_config::config_path(ctx);
    let conf = conf.to_string_lossy();
    // `{{ODOO_HOME}}` was substituted here and appears nowhere in the template:
    // a dead entry, removed rather than kept, because "home" now has two
    // meanings (the shared root and the instance user's home) and leaving the
    // token available would invite the next person to reach for the wrong one.
    let replacements: [(&str, &str); 8] = [
        ("{{ODOO_VERSION}}", ctx.odoo_version.as_str()),
        // the syslog identity and the config file are named after the
        // **instance**: two instances of the same Odoo version would otherwise
        // share both.
        ("{{INSTANCE_BASE}}", base.as_str()),
        // `RuntimeDirectory` takes the other family, so the unnamed instance
        // keeps `/run/odoo` exactly as before. it matters more than it looks:
        // systemd removes that directory when the service stops, so two units
        // declaring the same one would have each stop pull the ground from
        // under the other.
        ("{{INSTANCE_QUALIFIED}}", qualified.as_str()),
        ("{{ODOO_USER}}", ctx.odoo_user.as_str()),
        ("{{ODOO_INSTALL_DIR}}", install.as_ref()),
        ("{{ODOO_CONF}}", conf.as_ref()),
        ("{{ODOO_REPO_DIR}}", REPO_SUBDIR),
        ("{{ODOO_VENV_DIR}}", VENV_SUBDIR),
    ];
    let mut out = SERVICE_TEMPLATE.to_string();
    for (token, value) in replacements {
        out = out.replace(token, value);
    }
    out
}

/// checks that no placeholder is left in the rendered unit.
pub fn validate_unit(content: &str) -> Result<(), StepError> {
    if content.contains("{{") {
        return Err(StepError::Precondition(
            "systemd unit: unsubstituted {{...}} placeholders".to_string(),
        ));
    }
    Ok(())
}
