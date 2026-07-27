//! [`GenerateConfig`]: genera `odoo<N>.conf` dal template, in modo reversibile.
//!
//! Due novità rispetto agli step precedenti:
//! - **Undo "ripristinante"**: nel caso `Preexisting` l'undo non rimuove il file,
//!   ne **ripristina il backup** — il file originale del cliente torna al suo
//!   posto (non sovrascritto, non cancellato).
//! - **Master password mai world-readable**: il file nasce in un temp privato
//!   (mode `0600`, owned root), poi viene spostato a destinazione e reso `0640`
//!   `odoo:odoo`. In nessun istante la password è leggibile da altri utenti.
//!
//! Il template è **embedded** nel binario (`include_str!`): nessuna dipendenza
//! da file esterni a runtime.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

/// Template `odoo.conf`, incorporato nel binario.
const CONFIG_TEMPLATE: &str = include_str!("../../templates/odoo.conf.tpl");
const CONF_MODE: u32 = 0o640;

/// Snapshot serializzabile.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GenerateConfigSnapshot {
    pub prestate: PreState,
    /// Path del backup creato quando si sovrascrive un file preesistente.
    pub backup_path: Option<String>,
}

/// Genera `odoo<N>.conf` (reversibile).
pub struct GenerateConfig {
    ops: Box<dyn SystemOps>,
    snap: GenerateConfigSnapshot,
}

impl GenerateConfig {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: GenerateConfigSnapshot::default(),
        }
    }

    fn dest(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir
            .join(format!("odoo{}.conf", ctx.odoo_version_short))
    }

    /// Path del file temporaneo privato, nella **stessa** directory della
    /// destinazione (stesso filesystem → rename atomico).
    ///
    /// Il nome è imprevedibile e il file viene creato con `O_EXCL | O_NOFOLLOW`
    /// ([`SystemOps::create_private_file`]): la install dir è posseduta
    /// dall'utente `odoo` mentre `run` gira come **root**, quindi un path
    /// prevedibile permetterebbe di pre-piazzarci un symlink e farci scrivere
    /// attraverso — sia un file di sistema arbitrario, sia il contenuto reso,
    /// che porta le password.
    fn temp_path(dest: &std::path::Path) -> std::path::PathBuf {
        crate::system_ops::private_temp_path(dest, "odoo.conf")
    }
}

impl Default for GenerateConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for GenerateConfig {
    fn name(&self) -> &str {
        "generate-config"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let dest = Self::dest(ctx);
        self.snap.prestate = if self.ops.path_exists(&dest) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(conf = %dest.display(), prestate = ?self.snap.prestate, "snapshot generate-config");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry-run): renderizzerei odoo.conf (temp privato → move → chown 640)");
            return Ok(());
        }

        let dest = Self::dest(ctx);

        // Backup se sovrascriviamo un file esistente (per un undo ripristinante).
        if self.snap.prestate == PreState::Preexisting {
            let backup = format!("{}.bak.{}", dest.display(), unix_timestamp());
            self.ops.copy_file(&dest, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            warn!(backup = %backup, "run: file esistente, backup creato prima di sovrascrivere");
        }

        // Rendering + validazione (puri; il plaintext della password vive solo
        // dentro `content`, mai loggato).
        let content = render_config(CONFIG_TEMPLATE, ctx);
        validate_rendered(&content)?;

        // Scrittura privata → move → ownership. La password non è mai in un file
        // world-readable in nessun momento.
        let tmp = Self::temp_path(&dest);
        self.ops.create_private_file(&tmp, &content)?;
        // Il temp ha un nome casuale: se il move fallisce nessuno lo
        // riconoscerebbe più come nostro, quindi lo rimuoviamo subito (contiene
        // le password: non va lasciato in giro).
        if let Err(e) = self.ops.move_file(&tmp, &dest) {
            let _ = self.ops.remove_file(&tmp);
            return Err(e);
        }
        self.ops
            .chown_named(&dest, &ctx.odoo_user, &ctx.odoo_user)?;
        self.ops.chmod(&dest, CONF_MODE)?;

        if self.snap.prestate == PreState::Untracked {
            self.snap.prestate = PreState::CreatedByUs;
        }
        info!(conf = %dest.display(), "run: odoo.conf generato (0640 odoo:odoo)");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rm o ripristino del backup secondo il PreState");
            return Ok(());
        }
        let dest = Self::dest(ctx);
        match self.snap.prestate {
            PreState::CreatedByUs => {
                // L'abbiamo creato noi: rm -f.
                if let Err(e) = self.ops.remove_file(&dest) {
                    warn!(error = %e, "undo: rimozione odoo.conf fallita, proseguo (best-effort)");
                } else {
                    info!(conf = %dest.display(), "undo: odoo.conf rimosso");
                }
            }
            PreState::Preexisting => {
                // Ripristina il file originale del cliente dal backup.
                match &self.snap.backup_path {
                    Some(backup) => {
                        let backup_path = std::path::Path::new(backup);
                        if self.ops.path_exists(backup_path) {
                            if let Err(e) = self.ops.move_file(backup_path, &dest) {
                                warn!(error = %e, "undo: ripristino backup fallito, proseguo (best-effort)");
                            } else {
                                info!(conf = %dest.display(), "undo: odoo.conf originale ripristinato dal backup");
                            }
                        } else {
                            warn!(backup = %backup, "undo: backup non trovato, non ripristino (best-effort)");
                        }
                    }
                    None => warn!("undo: nessun backup registrato, non ripristino"),
                }
            }
            PreState::Untracked => {
                info!("undo NO-OP (config non generata da noi)");
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }
}

/// Secondi dall'epoch (per il nome del backup). In codice normale è lecito.
fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Renderizza il template sostituendo i `${VAR}` con i valori del Context, poi
/// normalizza le direttive rimaste vuote a `False` (comportamento del Bash,
/// atteso da Odoo). La password in chiaro entra qui e **solo qui**.
pub fn render_config(template: &str, ctx: &Context) -> String {
    let install = ctx.install_dir.to_string_lossy();
    let addons =
        format!("{install}/odoo/odoo/addons,{install}/odoo/addons,{install}/repos/modules");
    let data_dir = format!("{}/.local/share/Odoo", ctx.odoo_home.to_string_lossy());
    let logfile = ctx
        .odoo_logfile
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let proxy_mode = if ctx.with_nginx { "True" } else { "False" };

    // Unico varco al plaintext delle password (admin + db).
    let admin = ctx.admin_passwd.expose();
    let db_password = ctx.db_password.expose();

    let port = ctx.port.to_string();
    let replacements: [(&str, &str); 21] = [
        ("ODOO_VERSION", ctx.odoo_version.as_str()),
        ("ODOO_ADDONS_PATH", addons.as_str()),
        ("ODOO_ADMIN_PASSWD", admin),
        ("ODOO_DATA_DIR", data_dir.as_str()),
        ("DB_HOST", ""),
        ("DB_NAME", ctx.db_name.as_str()),
        ("DB_PASSWORD", db_password),
        ("DB_PORT", ""),
        ("DB_USER", ctx.db_user.as_str()),
        ("ODOO_HTTP_INTERFACE", "0.0.0.0"),
        ("ODOO_PORT", port.as_str()),
        ("ODOO_LIMIT_MEMORY_HARD", "2684354560"),
        ("ODOO_LIMIT_MEMORY_SOFT", "2147483648"),
        ("ODOO_LIMIT_REQUEST", "8192"),
        ("ODOO_LIMIT_TIME_CPU", "60"),
        ("ODOO_LIMIT_TIME_REAL", "120"),
        ("ODOO_LOG_LEVEL", "info"),
        ("ODOO_LOGFILE", logfile.as_str()),
        ("ODOO_MAX_CRON_THREADS", "1"),
        ("ODOO_PROXY_MODE", proxy_mode),
        ("ODOO_WORKERS", "0"),
    ];

    let mut out = template.to_string();
    for (key, value) in replacements {
        out = out.replace(&format!("${{{key}}}"), value);
    }

    normalize_empty_directives(&out)
}

/// Rende `False` ogni direttiva `key =` rimasta senza valore (Odoo rifiuta i
/// valori vuoti). Non tocca commenti (`;`) né direttive con valore.
pub fn normalize_empty_directives(content: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim();
            let value = line[eq + 1..].trim();
            let is_directive = !key.is_empty()
                && !key.starts_with(';')
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if is_directive && value.is_empty() {
                out.push(format!("{} False", &line[..=eq]));
                continue;
            }
        }
        out.push(line.to_string());
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Validazione del `.conf` prodotto: `[options]`, `addons_path` e `http_port`
/// valorizzati, nessun placeholder `${...}` residuo.
pub fn validate_rendered(content: &str) -> Result<(), StepError> {
    if !content.lines().any(|l| l.trim() == "[options]") {
        return Err(StepError::Precondition(
            "odoo.conf: sezione [options] mancante".to_string(),
        ));
    }
    if !has_valued_directive(content, "addons_path") {
        return Err(StepError::Precondition(
            "odoo.conf: addons_path non valorizzato".to_string(),
        ));
    }
    if !has_valued_directive(content, "http_port") {
        return Err(StepError::Precondition(
            "odoo.conf: http_port non valorizzato".to_string(),
        ));
    }
    if content.contains("${") {
        return Err(StepError::Precondition(
            "odoo.conf: placeholder non sostituiti".to_string(),
        ));
    }
    Ok(())
}

/// `true` se esiste una riga `key = <valore non vuoto>`.
fn has_valued_directive(content: &str, key: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(eq) = rest.find('=') {
                return !rest[eq + 1..].trim().is_empty();
            }
        }
        false
    })
}
