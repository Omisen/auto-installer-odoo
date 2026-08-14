//! configuration resolution: the CLI → `.env` → interactive → default cascade.
//!
//! reproduces the *semantics* of Bash's `lib/cli.sh`, not its shape: the
//! `CLI_*_SET` booleans become `Option<T>`, the `source` of the `.env` file —
//! code execution as root — becomes a **declarative** parser
//! ([`parse_env_file`]), and `exit 1` becomes a typed [`ConfigError`].
//!
//! [`ResolvedConfig::resolve`] is **pure**: no I/O, no prompts. those live in
//! [`crate::prompt`] and fill another `RawConfig` layered over the `.env`.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::cli::Cli;
use crate::error::StepError;
use crate::secret::Secret;

// --- defaults and constants -------------------------------------------------

/// an architectural constant: not overridable.
pub const ODOO_HOME: &str = "/opt/odoo";
const DEFAULT_VERSION: &str = "18.0";
const DEFAULT_ODOO_USER: &str = "odoo";
const DEFAULT_PORT: &str = "8069";
const DEFAULT_DB_NAME: &str = "odoo";
const DEFAULT_ADMIN_PASSWD: &str = "admin";

// --- configuration errors ---------------------------------------------------

/// a configuration resolution or validation error.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid Odoo version: '{0}'. accepted values: 16|17|18|19 or 16.0..19.0")]
    InvalidVersion(String),

    #[error(
        "invalid {field}: '{value}'. use letters, digits, dot, dash or underscore only, and \
         start with a letter, a digit or an underscore (a name starting with '-' would be \
         taken for an option by the system commands)"
    )]
    InvalidIdentifier { field: &'static str, value: String },

    #[error("invalid Odoo port: '{0}'. enter a number between 1 and 65535")]
    InvalidPort(String),

    #[error("invalid install dir: '{0}'. enter an absolute path")]
    InstallDirNotAbsolute(PathBuf),

    #[error("invalid install dir: '{0}'. it must live under '{1}'")]
    InstallDirOutOfScope(PathBuf, String),

    #[error("the Odoo admin password cannot be empty")]
    EmptyPassword,

    #[error(
        "admin_passwd='admin' requires an explicit interactive confirmation. \
         set a different password, or run again in interactive mode"
    )]
    InsecureAdminNonInteractive,

    #[error("config file not found: {0}")]
    ConfigFileNotFound(PathBuf),

    #[error("cannot read the config file '{path}': {source}")]
    ConfigFileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// --- RawConfig: raw values from one source ----------------------------------

/// unvalidated values from one source: the CLI, the `.env` or the prompts.
///
/// every field is an `Option`, where `None` means this source did not supply
/// it. `admin_passwd` is held in the clear because it still has to be parsed,
/// so the struct deliberately does **not** implement `Debug`.
#[derive(Default, Clone)]
pub struct RawConfig {
    pub version: Option<String>,
    pub odoo_user: Option<String>,
    pub db_user: Option<String>,
    pub db_password: Option<String>,
    pub port: Option<String>,
    pub db_name: Option<String>,
    pub install_dir: Option<String>,
    pub admin_passwd: Option<String>,
    pub logfile: Option<String>,
    pub with_nginx: Option<bool>,
    pub server_name: Option<String>,
    /// opens 443 on the firewall; does not enable TLS.
    pub open_https_port: Option<bool>,
}

impl RawConfig {
    /// extracts the raw values from the parsed CLI arguments.
    pub fn from_cli(cli: &Cli) -> Self {
        RawConfig {
            version: cli.version.clone(),
            odoo_user: cli.odoo_user.clone(),
            db_user: cli.db_user.clone(),
            db_password: cli.db_password.clone(),
            port: cli.port.map(|p| p.to_string()),
            db_name: cli.db_name.clone(),
            install_dir: cli
                .install_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            admin_passwd: cli.admin_passwd.clone(),
            logfile: cli
                .logfile
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            // not passed → `None`, falling through to .env/default: there is no
            // way to negate it from the CLI, as in Bash.
            with_nginx: if cli.with_nginx { Some(true) } else { None },
            server_name: cli.server_name.clone(),
            open_https_port: if cli.open_https_port {
                Some(true)
            } else {
                None
            },
        }
    }
}

/// a **declarative** `.env` parser: `KEY=VALUE`, line by line.
///
/// blank lines and `#` comments are ignored, an `export` prefix is tolerated,
/// and quotes around the value are stripped. **nothing is executed**: a value
/// like `$(rm -rf /)` stays a literal string. unknown keys warn and are ignored
/// rather than failing.
///
/// # errors
///
/// [`ConfigError`] when the file cannot be read or a line has no `=`.
pub fn parse_env_file(path: &Path) -> Result<RawConfig, ConfigError> {
    let content = fs::read_to_string(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ConfigError::ConfigFileNotFound(path.to_path_buf())
        } else {
            ConfigError::ConfigFileRead {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;

    let mut raw = RawConfig::default();

    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // tolerate `export KEY=VALUE`.
        let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);

        let Some((key, value)) = line.split_once('=') else {
            warn!(line = lineno + 1, "skipped .env line (no '=')");
            continue;
        };

        let key = key.trim();
        let value = strip_quotes(value.trim());

        match key {
            "ODOO_VERSION" => raw.version = Some(value),
            "ODOO_USER" => raw.odoo_user = Some(value),
            "DB_USER" => raw.db_user = Some(value),
            "DB_PASSWORD" => raw.db_password = Some(value),
            "ODOO_PORT" => raw.port = Some(value),
            "DB_NAME" => raw.db_name = Some(value),
            "ODOO_INSTALL_DIR" => raw.install_dir = Some(value),
            "ODOO_ADMIN_PASSWD" => raw.admin_passwd = Some(value),
            "ODOO_LOGFILE" => raw.logfile = Some(value),
            "WITH_NGINX" => raw.with_nginx = Some(parse_bool(&value)),
            "NGINX_SERVER_NAME" => raw.server_name = Some(value),
            // the historical name, still honoured because it lives in
            // customers' `.env` files — but it promised TLS (A-V3-6).
            "NGINX_OPEN_HTTPS_PORT" | "NGINX_ENABLE_SSL" => {
                raw.open_https_port = Some(parse_bool(&value))
            }
            "ODOO_HOME" => warn!("ODOO_HOME is a fixed constant, .env key ignored"),
            other => warn!(key = other, "unknown .env key ignored"),
        }
    }

    Ok(raw)
}

/// strips one pair of quotes wrapping the whole value. nothing else is
/// interpreted.
fn strip_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// reads a textual boolean: `true`/`1`/`yes`/`on`, case-insensitive.
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

// --- validators (pure, testable) --------------------------------------------

/// normalises the Odoo version, returning `(full, short)` — `("18.0", "18")`.
pub fn normalize_version(value: &str) -> Result<(String, String), ConfigError> {
    let full = match value {
        "16" | "17" | "18" | "19" => format!("{value}.0"),
        "16.0" | "17.0" | "18.0" | "19.0" => value.to_string(),
        _ => return Err(ConfigError::InvalidVersion(value.to_string())),
    };
    let short = full.split('.').next().unwrap_or(full.as_str()).to_string();
    Ok((full, short))
}

/// `true` when `value` is a valid identifier (`^[A-Za-z0-9_][A-Za-z0-9._-]*$`).
///
/// the **first** character must be alphanumeric or `_`, and that is not
/// cosmetic: these names end up as positional arguments to `createdb`, `dropdb`
/// and `useradd`, where `-foo` would be read as a **flag** instead of an
/// operand (argument injection).
///
/// this is the upstream gate; the downstream net is the `--` before positionals
/// in [`crate::system_ops::argv`]. both are needed.
fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// validates an identifier, naming the field in the error.
pub fn validate_identifier(value: &str, field: &'static str) -> Result<String, ConfigError> {
    if is_valid_identifier(value) {
        Ok(value.to_string())
    } else {
        Err(ConfigError::InvalidIdentifier {
            field,
            value: value.to_string(),
        })
    }
}

/// validates a port: an integer in 1..=65535.
pub fn validate_port(value: &str) -> Result<u16, ConfigError> {
    let n: u32 = value
        .parse()
        .map_err(|_| ConfigError::InvalidPort(value.to_string()))?;
    if (1..=65535).contains(&n) {
        Ok(n as u16)
    } else {
        Err(ConfigError::InvalidPort(value.to_string()))
    }
}

/// resolves and validates the install dir: derived default, absolute, under
/// `home`.
pub fn resolve_install_dir(
    explicit: Option<&str>,
    home: &Path,
    version_short: &str,
) -> Result<PathBuf, ConfigError> {
    let dir = match explicit {
        Some(value) => PathBuf::from(value),
        None => home.join(format!("odoo{version_short}")),
    };

    if !dir.is_absolute() {
        return Err(ConfigError::InstallDirNotAbsolute(dir));
    }
    // `Path::starts_with` is component-based, so `/opt/odoofoo` is correctly
    // outside `/opt/odoo` — the string-prefix bug Bash had.
    if !dir.starts_with(home) {
        return Err(ConfigError::InstallDirOutOfScope(
            dir,
            home.to_string_lossy().into_owned(),
        ));
    }
    Ok(dir)
}

/// outcome of the master-password check.
#[derive(Debug, PartialEq, Eq)]
pub enum AdminConfirm {
    /// not `admin`: no confirmation needed.
    NotNeeded,
    /// `admin` interactively: an explicit y/N confirmation is required.
    ConfirmNeeded,
}

/// applies the master-password rule. pure, no I/O.
///
/// empty is an error; anything other than `admin` passes; `admin` without a TTY
/// is a hard stop, because it cannot be confirmed; `admin` with a TTY asks the
/// caller to confirm.
///
/// # errors
///
/// [`ConfigError`] for an empty password, or for `admin` non-interactively.
pub fn check_admin_password(
    password: &str,
    interactive: bool,
) -> Result<AdminConfirm, ConfigError> {
    if password.is_empty() {
        return Err(ConfigError::EmptyPassword);
    }
    if password != DEFAULT_ADMIN_PASSWD {
        return Ok(AdminConfirm::NotNeeded);
    }
    if !interactive {
        return Err(ConfigError::InsecureAdminNonInteractive);
    }
    Ok(AdminConfirm::ConfirmNeeded)
}

// --- resolved config --------------------------------------------------------

/// resolved and validated configuration, ready to build a
/// [`crate::context::Context`].
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub version: String,
    pub version_short: String,
    pub odoo_user: String,
    pub db_user: String,
    /// password of the PostgreSQL role; empty means peer auth.
    pub db_password: Secret,
    pub odoo_home: PathBuf,
    pub port: u16,
    pub db_name: String,
    pub install_dir: PathBuf,
    pub admin_passwd: Secret,
    /// Odoo's log file; `None` means journal/stdout.
    pub odoo_logfile: Option<PathBuf>,
    pub with_nginx: bool,
    pub nginx_server_name: String,
    /// opens 443 on the firewall. does **not** enable TLS in the vhost — that
    /// is `certbot --nginx`'s job (A-V3-6).
    pub nginx_open_https_port: bool,
}

/// picks the first value present, left to right.
fn pick(cli: &Option<String>, prompted: &Option<String>, env: &Option<String>) -> Option<String> {
    cli.clone()
        .or_else(|| prompted.clone())
        .or_else(|| env.clone())
}

impl ResolvedConfig {
    /// resolves the config over the **CLI → prompts → `.env` → default**
    /// cascade.
    ///
    /// pure: it runs no prompt and touches no disk. `prompted` is empty in
    /// non-interactive mode. confirming an `admin` password stays with the
    /// caller (see [`check_admin_password`]); only the non-interactive hard
    /// stop is applied here.
    ///
    /// # errors
    ///
    /// [`ConfigError`] when any field fails validation.
    pub fn resolve(
        cli: &RawConfig,
        env: &RawConfig,
        prompted: &RawConfig,
        interactive: bool,
    ) -> Result<Self, ConfigError> {
        let home = PathBuf::from(ODOO_HOME);

        let version_raw = pick(&cli.version, &prompted.version, &env.version)
            .unwrap_or_else(|| DEFAULT_VERSION.to_string());
        let (version, version_short) = normalize_version(&version_raw)?;

        let odoo_user_raw = pick(&cli.odoo_user, &prompted.odoo_user, &env.odoo_user)
            .unwrap_or_else(|| DEFAULT_ODOO_USER.to_string());
        let odoo_user = validate_identifier(&odoo_user_raw, "Odoo user")?;

        let db_name_raw = pick(&cli.db_name, &prompted.db_name, &env.db_name)
            .unwrap_or_else(|| DEFAULT_DB_NAME.to_string());
        let db_name = validate_identifier(&db_name_raw, "database name")?;

        let port_raw =
            pick(&cli.port, &prompted.port, &env.port).unwrap_or_else(|| DEFAULT_PORT.to_string());
        let port = validate_port(&port_raw)?;

        // follows odoo_user unless explicitly decoupled.
        let db_user = resolve_db_user(cli.db_user.as_deref(), env.db_user.as_deref(), &odoo_user)?;

        // empty or absent → peer auth.
        let db_password = Secret::new(
            pick(&cli.db_password, &prompted.db_password, &env.db_password).unwrap_or_default(),
        );

        let install_dir_raw = pick(&cli.install_dir, &prompted.install_dir, &env.install_dir);
        let install_dir = resolve_install_dir(install_dir_raw.as_deref(), &home, &version_short)?;

        let admin_raw = pick(&cli.admin_passwd, &prompted.admin_passwd, &env.admin_passwd)
            .unwrap_or_else(|| DEFAULT_ADMIN_PASSWD.to_string());
        // non-interactive hard stop; the interactive confirm is the caller's.
        check_admin_password(&admin_raw, interactive)?;

        // an empty string means disabled.
        let odoo_logfile = pick(&cli.logfile, &prompted.logfile, &env.logfile)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        let with_nginx = cli
            .with_nginx
            .or(prompted.with_nginx)
            .or(env.with_nginx)
            .unwrap_or(false);
        let nginx_server_name = pick(&cli.server_name, &prompted.server_name, &env.server_name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_".to_string());
        let nginx_open_https_port = cli
            .open_https_port
            .or(prompted.open_https_port)
            .or(env.open_https_port)
            .unwrap_or(false);

        Ok(ResolvedConfig {
            version,
            version_short,
            odoo_user,
            db_user,
            db_password,
            odoo_home: home,
            port,
            db_name,
            install_dir,
            admin_passwd: Secret::new(admin_raw),
            odoo_logfile,
            with_nginx,
            nginx_server_name,
            nginx_open_https_port,
        })
    }
}

/// applies the "db_user follows odoo_user" rule.
///
/// they stay coupled unless `db_user` came explicitly from the CLI, or from an
/// `.env` value different from the `odoo` default.
fn resolve_db_user(
    cli_db_user: Option<&str>,
    env_db_user: Option<&str>,
    odoo_user: &str,
) -> Result<String, ConfigError> {
    let explicit_from_cli = cli_db_user.is_some();
    match cli_db_user.or(env_db_user) {
        None => Ok(odoo_user.to_string()),
        Some(value) => {
            if !explicit_from_cli && value == DEFAULT_ODOO_USER {
                Ok(odoo_user.to_string())
            } else {
                validate_identifier(value, "database user")
            }
        }
    }
}

// lets a config error cross into the engine without coupling the two modules.
impl From<ConfigError> for StepError {
    fn from(err: ConfigError) -> Self {
        StepError::Precondition(err.to_string())
    }
}
