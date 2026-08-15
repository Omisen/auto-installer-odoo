//! the [`Context`]: resolved configuration handed to every step.
//!
//! steps read **only** from here: they do not know whether a value came from a
//! CLI flag, a `.env` file or an interactive prompt (resolution happens in
//! [`crate::config`]). that decoupling is what lets one installer run
//! interactively or not through a single flow.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::checks::{OsInfo, PythonPlan};
use crate::config::ResolvedConfig;
use crate::distro::OsFamily;
use crate::secret::Secret;

/// resolved installation config plus the engine's runtime state.
///
/// `admin_passwd` is a [`Secret`]: its `Debug` is redacted, so logging a
/// `Context` never exposes the password.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// full Odoo version, e.g. `"18.0"`.
    pub odoo_version: String,
    /// short Odoo version, e.g. `"18"`.
    ///
    /// no longer the thing that names artifacts: that is
    /// [`Context::artifact_base`], which for the unnamed instance still produces
    /// `odoo18` from this value.
    pub odoo_version_short: String,
    /// name of this instance, or `None` for the historical unnamed one.
    ///
    /// stored **raw**, and every derived name is computed from it on demand
    /// rather than stored alongside: two fields that must agree is how this
    /// project's defects are shaped, and there is no reason to keep a copy of
    /// something a `format!` reproduces.
    pub instance: Option<String>,
    /// system user that will own the installation.
    pub odoo_user: String,
    /// PostgreSQL role.
    pub db_user: String,
    /// password of the PostgreSQL role; empty means peer auth.
    pub db_password: Secret,
    /// the **shared** root: the constant `/opt/odoo`.
    ///
    /// shared by every instance on the machine, which is why it is not the same
    /// thing as the home of the user running Odoo — see [`Context::user_home`].
    pub odoo_home: PathBuf,
    /// Odoo's HTTP port.
    pub port: u16,
    /// Odoo database name.
    pub db_name: String,
    /// install directory, always under `odoo_home`.
    pub install_dir: PathBuf,
    /// Odoo master password, redacted in the logs.
    pub admin_passwd: Secret,
    /// Odoo log file; `None` logs to journal/stdout. decides whether
    /// [`crate::steps::setup_log_dir`] creates a directory or is a no-op.
    pub odoo_logfile: Option<PathBuf>,
    /// whether to configure Nginx as a reverse proxy.
    pub with_nginx: bool,
    /// `server_name` of the Nginx vhost, `_` by default.
    pub nginx_server_name: String,
    /// opens 443 on the firewall; it does **not** enable TLS (A-V3-6).
    pub nginx_open_https_port: bool,
    /// when set, `run` and `undo` must neither mutate nor persist state.
    pub dry_run: bool,
    /// when set, the rollback also purges what it would normally leave: common
    /// bootstrap utilities and PostgreSQL.
    pub aggressive_rollback: bool,
    /// another instance is still installed on this machine (phase I2).
    ///
    /// rollback-wide policy, like [`Self::aggressive_rollback`], and read by the
    /// two steps whose undo is **partly** shared: they do their own half and
    /// leave the shared one alone. the steps that are wholly shared never get
    /// called at all — that decision is the rollback driver's, from
    /// [`crate::steps::artifact_scope`].
    ///
    /// `false` by default, so every path that does not set it behaves exactly as
    /// it did before instances existed.
    pub shared_in_use: bool,
    /// path of the persisted state file; configurable for the tests.
    pub state_path: PathBuf,
    /// OS details from the preflight checks, filled in after `check_os`.
    ///
    /// stays an `Option` because it is what it has always been: detail needed
    /// **only** by `run`. the family, which the undos need too, lives in
    /// [`Self::os_family`] and is not optional — see there for why.
    pub os_info: Option<OsInfo>,
    /// which commands and conventions apply on this machine.
    ///
    /// **not an `Option`, deliberately**: every path has an answer — the system
    /// while installing, the manifest while rolling back. an `Option` would
    /// reopen the hole this field closes, leaving the delta's undo without a
    /// package manager to invoke. the family is **re-read**, not re-derived —
    /// see [`crate::distro::OsFamily`].
    pub os_family: OsFamily,
    /// user who ran `sudo` (`SUDO_USER`), owner of the control script and of
    /// the patched `.bashrc`.
    pub sudo_user: Option<String>,
    /// shared verdict published by `CreateDatabase` and read by
    /// `InitializeOdooDatabase`: `true` means the database is ours, so
    /// initialising its schema is allowed.
    ///
    /// defaults to `false` = refuse, which is the safe direction: without
    /// propagation the init is forbidden. interior mutability lets a step set
    /// it while holding `&Context`.
    pub db_created_by_us: Arc<AtomicBool>,
    /// interpreter the virtualenv will be built on, and the packages that
    /// provide it.
    ///
    /// **configuration, not a step result** (M11): decided at preflight by
    /// [`crate::checks::plan_python`], like `os_family`. two readers need one
    /// answer — one installs the interpreter, the other invokes it — and
    /// deriving it twice is how two readings diverge in silence.
    ///
    /// the `Default` is `python3` with no packages, so any path that decides
    /// nothing behaves as it did before M11.
    pub python: PythonPlan,
}

impl Context {
    /// builds the context from the resolved config, adding the engine's runtime
    /// state (`dry_run`, `state_path`).
    pub fn from_resolved(config: ResolvedConfig, dry_run: bool, state_path: PathBuf) -> Self {
        Context {
            odoo_version: config.version,
            odoo_version_short: config.version_short,
            instance: config.instance,
            odoo_user: config.odoo_user,
            db_user: config.db_user,
            db_password: config.db_password,
            odoo_home: config.odoo_home,
            port: config.port,
            db_name: config.db_name,
            install_dir: config.install_dir,
            admin_passwd: config.admin_passwd,
            odoo_logfile: config.odoo_logfile,
            with_nginx: config.with_nginx,
            nginx_server_name: config.nginx_server_name,
            nginx_open_https_port: config.nginx_open_https_port,
            dry_run,
            aggressive_rollback: false,
            shared_in_use: false,
            state_path,
            os_info: None,
            // both are filled in by `main` at preflight; before that no step
            // runs, and the defaults are the pre-M11 behaviour.
            os_family: OsFamily::default(),
            sudo_user: None,
            db_created_by_us: Arc::new(AtomicBool::new(false)),
            python: PythonPlan::default(),
        }
    }

    /// overrides the state file path, for the tests and for the resume path.
    pub fn with_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_path = path.into();
        self
    }

    /// the base name of this instance's versioned artifacts: unit, config file,
    /// vhost, install dir.
    ///
    /// `odoo18` unnamed — what every release so far wrote — `odoo-<name>` for a
    /// named instance. steps ask this instead of formatting the version
    /// themselves, so the naming rule has one home
    /// ([`crate::instance::artifact_base`]) and not eleven copies.
    pub fn artifact_base(&self) -> String {
        crate::instance::artifact_base(self.instance.as_deref(), &self.odoo_version_short)
    }

    /// this instance's name for the artifacts that are plain `odoo` today: the
    /// runtime directory, and the default for the user, the role, the database
    /// and the helper command.
    ///
    /// distinct from [`Self::artifact_base`] only for the unnamed instance —
    /// `odoo` against `odoo18` — and that difference is the whole reason both
    /// exist: `/run/odoo` and the `odoo` command must keep their names, while
    /// the unit and the config file must keep theirs.
    pub fn qualified_name(&self) -> String {
        crate::instance::qualified_name(self.instance.as_deref())
    }

    /// the home of the user Odoo runs as — **not** the same as
    /// [`Self::odoo_home`].
    ///
    /// unnamed instance: `/opt/odoo`, exactly as before, shared with whatever
    /// else lives under it. named instance: the install dir, so that the
    /// filestore, the cache and everything else Odoo writes into its home lands
    /// inside that instance's own perimeter — and stays unreadable to the other
    /// instances, whose users are different (§ 9.2 of `audit-v6`, option α).
    ///
    /// the distinction is what makes isolation real rather than nominal: with a
    /// single shared home and a single shared user, per-instance PostgreSQL
    /// roles would separate nothing, since one instance could read the other's
    /// `odoo.conf` and with it the password.
    pub fn user_home(&self) -> PathBuf {
        match self.instance {
            Some(_) => self.install_dir.clone(),
            None => self.odoo_home.clone(),
        }
    }
}
