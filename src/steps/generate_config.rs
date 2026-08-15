//! [`GenerateConfig`]: renders `odoo<N>.conf` from the template, reversibly.
//!
//! two novelties over the earlier steps:
//! - a **restoring undo**: on `Preexisting` it does not remove the file but
//!   **puts the backup back**, so the customer's original returns unchanged;
//! - the **master password is never world-readable**: the file is born in a
//!   private temporary and only then moved into place and made group-readable
//!   by `odoo`.
//!
//! the template is **embedded** in the binary, so nothing external is needed at
//! runtime.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::SystemOps;

/// the `odoo.conf` template, embedded in the binary.
const CONFIG_TEMPLATE: &str = include_str!("../../templates/odoo.conf.tpl");
const CONF_MODE: u32 = 0o640;

/// the serialisable snapshot.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GenerateConfigSnapshot {
    pub prestate: PreState,
    /// path of the backup taken when overwriting a pre-existing file.
    pub backup_path: Option<String>,
}

/// renders `odoo<N>.conf`, reversibly.
pub struct GenerateConfig {
    ops: Box<dyn SystemOps>,
    snap: GenerateConfigSnapshot,
}

impl GenerateConfig {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: GenerateConfigSnapshot::default(),
        }
    }

    fn dest(ctx: &Context) -> std::path::PathBuf {
        config_path(ctx)
    }

    /// the private temporary's path, in the **same** directory as the
    /// destination so the final move is an atomic rename.
    ///
    /// the name is unpredictable and the file is created fail-closed: the
    /// install dir belongs to `odoo` while `run` is **root**, so a predictable
    /// path would let a symlink be planted and written through — either onto an
    /// arbitrary system file, or to capture the rendered contents, which carry
    /// the passwords.
    fn temp_path(dest: &std::path::Path) -> std::path::PathBuf {
        crate::system_ops::private_temp_path(dest, "odoo.conf")
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
        info!(conf = %dest.display(), prestate = ?self.snap.prestate, "snapshot: generate-config");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry run): would render odoo.conf (private temp, move, chown 640)");
            return Ok(());
        }

        let dest = Self::dest(ctx);

        // back up an existing file, so the undo can restore it.
        if self.snap.prestate == PreState::Preexisting {
            let backup = format!("{}.bak.{}", dest.display(), unix_timestamp());
            self.ops.copy_file(&dest, std::path::Path::new(&backup))?;
            self.snap.backup_path = Some(backup.clone());
            warn!(backup = %backup, "run: the file existed, a backup was made before overwriting");
        }

        // render and validate; the plaintext password lives only inside
        // `content` and is never logged.
        let content = render_config(CONFIG_TEMPLATE, ctx);
        validate_rendered(&content)?;

        // private write → move → ownership: the password is never in a
        // world-readable file at any instant.
        let tmp = Self::temp_path(&dest);
        self.ops.create_private_file(&tmp, &content)?;
        // the temporary has a random name, so a failed move would leave
        // something nobody could recognise as ours — and it holds the
        // passwords.
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
        info!(conf = %dest.display(), "run: odoo.conf generated (0640 odoo:odoo)");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry run): remove, or restore the backup, according to the PreState");
            return Ok(());
        }
        let dest = Self::dest(ctx);
        match self.snap.prestate {
            PreState::CreatedByUs => {
                // ours: remove it.
                if let Err(e) = self.ops.remove_file(&dest) {
                    warn!(error = %e, "undo: removing odoo.conf failed, proceeding (best-effort)");
                } else {
                    info!(conf = %dest.display(), "undo: odoo.conf removed");
                }
            }
            PreState::Preexisting => {
                // put the customer's original back.
                match &self.snap.backup_path {
                    Some(backup) => {
                        let backup_path = std::path::Path::new(backup);
                        if self.ops.path_exists(backup_path) {
                            if let Err(e) = self.ops.move_file(backup_path, &dest) {
                                warn!(error = %e, "undo: restoring the backup failed, proceeding (best-effort)");
                            } else {
                                info!(conf = %dest.display(), "undo: the original odoo.conf was restored from the backup");
                            }
                        } else {
                            warn!(backup = %backup, "undo: backup not found, not restoring (best-effort)");
                        }
                    }
                    None => warn!("undo: no backup recorded, not restoring"),
                }
            }
            PreState::Untracked => {
                info!("undo NO-OP (config not generated by us)");
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// rehydrates the `backup_path` too: without it an undo on `Preexisting`
    /// would not know where to restore the customer's file from, and would
    /// leave ours in place with a warning.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}

/// where this instance's `odoo.conf` lives: `<install_dir>/<instance>.conf`.
///
/// **one** function, and the reason is the one already written for
/// [`data_dir`]: this path had three authors — this step, which writes the file;
/// [`InitializeOdooDatabase`](crate::steps::initialize_odoo_database), which
/// passes it to `odoo-bin`; and the systemd unit, which composed it inside the
/// template. three `format!`s that must agree, where a disagreement does not
/// fail loudly: the service simply starts against a config file that is not
/// there, or `odoo-bin -i base` initialises with different settings than the
/// service will run with.
///
/// I0 gave that a way to actually happen — the name stopped being a function of
/// the version alone — so the three became one and the unit now receives the
/// finished path instead of building its own.
pub fn config_path(ctx: &Context) -> std::path::PathBuf {
    ctx.install_dir
        .join(format!("{}.conf", ctx.artifact_base()))
}

/// Odoo's `data_dir`: where the **filestore** and the sessions live.
///
/// here because this step writes it into the config, but no longer only a
/// template value: [`SetupDataDir`](crate::steps::setup_data_dir) creates that
/// directory reversibly and must create *exactly* the path Odoo will use. two
/// identical `format!`s in two files are the premise of a rollback cleaning the
/// wrong directory.
pub fn data_dir(ctx: &Context) -> std::path::PathBuf {
    // the **user's** home, not the shared root: for a named instance those are
    // different directories, and that is what keeps two instances' attachments
    // apart. the derivation itself does not change — Odoo has always put the
    // filestore under the home of the user it runs as.
    ctx.user_home().join(".local").join("share").join("Odoo")
}

/// renders the template, then normalises directives left empty to `False`,
/// which is what Odoo expects.
///
/// the plaintext password enters here and **only** here.
pub fn render_config(template: &str, ctx: &Context) -> String {
    let install = ctx.install_dir.to_string_lossy();
    let addons =
        format!("{install}/odoo/odoo/addons,{install}/odoo/addons,{install}/repos/modules");
    let data_dir = data_dir(ctx);
    let data_dir = data_dir.to_string_lossy();
    let logfile = ctx
        .odoo_logfile
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let proxy_mode = if ctx.with_nginx { "True" } else { "False" };

    // the single opening onto the plaintext passwords.
    let admin = ctx.admin_passwd.expose();
    let db_password = ctx.db_password.expose();

    let port = ctx.port.to_string();
    let replacements: [(&str, &str); 21] = [
        ("ODOO_VERSION", ctx.odoo_version.as_str()),
        ("ODOO_ADDONS_PATH", addons.as_str()),
        ("ODOO_ADMIN_PASSWD", admin),
        ("ODOO_DATA_DIR", &data_dir),
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

/// turns every valueless `key =` directive into `key = False`, which Odoo
/// requires. comments and directives with values are untouched.
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

/// validates the rendered config: the section is present, the required
/// directives have values, and no placeholder is left.
pub fn validate_rendered(content: &str) -> Result<(), StepError> {
    if !content.lines().any(|l| l.trim() == "[options]") {
        return Err(StepError::Precondition(
            "odoo.conf: the [options] section is missing".to_string(),
        ));
    }
    if !has_valued_directive(content, "addons_path") {
        return Err(StepError::Precondition(
            "odoo.conf: addons_path has no value".to_string(),
        ));
    }
    if !has_valued_directive(content, "http_port") {
        return Err(StepError::Precondition(
            "odoo.conf: http_port has no value".to_string(),
        ));
    }
    if content.contains("${") {
        return Err(StepError::Precondition(
            "odoo.conf: unsubstituted placeholders".to_string(),
        ));
    }
    Ok(())
}

/// `true` when a `key = <non-empty value>` line exists.
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
