//! [`SetupSystemd`]: installa e attiva il servizio systemd di Odoo.
//!
//! Fase di consolidamento: riusa la struttura a **tre PreState indipendenti** di
//! [`SetupPostgres`](crate::steps::setup_postgres) (unit-file / enabled / active)
//! con la stessa regola D4, e il rendering da **template embedded** di
//! [`GenerateConfig`](crate::steps::generate_config). Nessuna protezione di dati
//! e nessuno stato condiviso: i tre assi sono locali allo step.
//!
//! Cura specifica: l'**ordine dell'undo** — stop → disable → rm → reload —
//! così systemd non resta con riferimenti a un'unit sparita.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// Template dell'unit, incorporato nel binario.
const SERVICE_TEMPLATE: &str = include_str!("../../templates/odoo.service.tpl");
const UNIT_MODE: u32 = 0o644;
const REPO_SUBDIR: &str = "odoo";
const VENV_SUBDIR: &str = "sandbox";
/// Attesa di assestamento dopo lo start, in produzione.
const DEFAULT_SETTLE_SECS: u64 = 3;

/// Snapshot dei tre assi indipendenti del servizio.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemdSnapshot {
    pub unit_file: PreState,
    pub enabled: PreState,
    pub active: PreState,
}

/// Installa/abilita/avvia il servizio systemd, ripristinando ogni asse.
pub struct SetupSystemd {
    ops: Box<dyn SystemOps>,
    settle_secs: u64,
    snap: SystemdSnapshot,
}

impl SetupSystemd {
    /// Costruttore di **produzione**: attende che il servizio si assesti prima di
    /// verificarne lo stato.
    ///
    /// [`Self::with_ops`] azzera l'attesa perché i test non devono dormire; usarlo
    /// per il `run` farebbe interrogare `is-active` nell'istante stesso dello
    /// start, cioè quando un servizio reale non è ancora salito.
    pub fn for_run(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            settle_secs: DEFAULT_SETTLE_SECS,
            snap: SystemdSnapshot::default(),
        }
    }

    /// Costruttore per i test: `SystemOps` iniettabile, nessuna attesa.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            settle_secs: 0,
            snap: SystemdSnapshot::default(),
        }
    }

    fn unit_name(ctx: &Context) -> String {
        format!("odoo{}", ctx.odoo_version_short)
    }

    fn unit_path(ctx: &Context) -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/etc/systemd/system/odoo{}.service",
            ctx.odoo_version_short
        ))
    }

    /// Temporaneo privato accanto alla unit (stesso filesystem → rename
    /// atomico), con nome imprevedibile e creazione `O_EXCL | O_NOFOLLOW`.
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
            "snapshot setup-systemd"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry-run): renderizzerei l'unit, installerei, enable + start");
            return Ok(());
        }

        let unit = Self::unit_name(ctx);
        let unit_path = Self::unit_path(ctx);

        // Rendering + validazione dell'unit.
        let content = render_unit(ctx);
        validate_unit(&content)?;

        // Installa (idempotente: sovrascrive). Il file governa solo l'undo.
        let tmp = Self::temp_path(&unit_path);
        self.ops.create_private_file(&tmp, &content)?;
        // Temp con nome casuale in /etc/systemd/system, che il rollback non
        // ripulisce: se il move fallisce, lo togliamo di mezzo noi.
        if let Err(e) = self.ops.move_file(&tmp, &unit_path) {
            let _ = self.ops.remove_file(&tmp);
            return Err(e);
        }
        self.ops.chmod(&unit_path, UNIT_MODE)?;
        self.ops.chown_named(&unit_path, "root", "root")?;
        if self.snap.unit_file == PreState::Untracked {
            self.snap.unit_file = PreState::CreatedByUs;
        }
        self.ops.daemon_reload()?;

        // Enable solo se non già abilitato (D4).
        if self.snap.enabled == PreState::Untracked {
            self.ops.service_enable(&unit)?;
            self.snap.enabled = PreState::CreatedByUs;
        }

        // Start (o restart se già attivo, per applicare la nuova config).
        if self.snap.active == PreState::Preexisting {
            self.ops.service_restart(&unit)?;
        } else {
            self.ops.service_start(&unit)?;
            self.snap.active = PreState::CreatedByUs;
        }

        // Assestamento + verifica.
        self.settle();
        if !self.ops.service_is_active(&unit) {
            return Err(StepError::Precondition(format!(
                "il servizio '{unit}' non risulta attivo dopo lo start. \
                 Controlla: journalctl -u {unit} -n 50 --no-pager"
            )));
        }
        info!(unit = %unit, "run: servizio systemd installato e attivo");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): stop → disable → rm → daemon-reload secondo lo snapshot");
            return Ok(());
        }

        let unit = Self::unit_name(ctx);
        let unit_path = Self::unit_path(ctx);

        // Ordine cruciale: stop → disable → rm → reload.

        // stop solo se l'avevamo avviato noi (D4).
        if self.snap.active == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_stop(&unit) {
                warn!(error = %e, "undo: stop servizio fallito, proseguo (best-effort)");
            }
        }
        // disable solo se l'avevamo abilitato noi (D4).
        if self.snap.enabled == PreState::CreatedByUs {
            if let Err(e) = self.ops.service_disable(&unit) {
                warn!(error = %e, "undo: disable servizio fallito, proseguo (best-effort)");
            }
        }
        // rimuovi l'unit file solo se l'abbiamo creato noi.
        if self.snap.unit_file == PreState::CreatedByUs {
            if let Err(e) = self.ops.remove_file(&unit_path) {
                warn!(error = %e, "undo: rimozione unit file fallita, proseguo (best-effort)");
            }
        }
        // reload finale: systemd dimentica l'unit rimossa.
        if let Err(e) = self.ops.daemon_reload() {
            warn!(error = %e, "undo: daemon-reload fallito, proseguo (best-effort)");
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

/// Renderizza l'unit dal template embedded sostituendo i `{{VAR}}`.
pub fn render_unit(ctx: &Context) -> String {
    let install = ctx.install_dir.to_string_lossy();
    let home = ctx.odoo_home.to_string_lossy();
    let replacements: [(&str, &str); 7] = [
        ("{{ODOO_VERSION}}", ctx.odoo_version.as_str()),
        ("{{ODOO_VERSION_SHORT}}", ctx.odoo_version_short.as_str()),
        ("{{ODOO_USER}}", ctx.odoo_user.as_str()),
        ("{{ODOO_HOME}}", home.as_ref()),
        ("{{ODOO_INSTALL_DIR}}", install.as_ref()),
        ("{{ODOO_REPO_DIR}}", REPO_SUBDIR),
        ("{{ODOO_VENV_DIR}}", VENV_SUBDIR),
    ];
    let mut out = SERVICE_TEMPLATE.to_string();
    for (token, value) in replacements {
        out = out.replace(token, value);
    }
    out
}

/// Verifica che nessun placeholder `{{...}}` sia rimasto nell'unit renderizzato.
pub fn validate_unit(content: &str) -> Result<(), StepError> {
    if content.contains("{{") {
        return Err(StepError::Precondition(
            "unit systemd: placeholder {{...}} non sostituiti".to_string(),
        ));
    }
    Ok(())
}
