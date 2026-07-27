//! Risoluzione della configurazione: cascata CLI → `.env` → interattivo → default.
//!
//! Replica la *semantica* di `lib/cli.sh` del Bash originale, non la forma:
//! - i booleani `CLI_*_SET` diventano `Option<T>` in [`RawConfig`];
//! - il `source` del file `.env` (esecuzione come root!) diventa un parser
//!   **dichiarativo** ([`parse_env_file`]) che non esegue nulla;
//! - gli `error`/`exit 1` diventano [`ConfigError`] tipizzati.
//!
//! La risoluzione ([`ResolvedConfig::resolve`]) è **pura**: nessun I/O, nessun
//! prompt. I prompt interattivi vivono in [`crate::prompt`] e riempiono un
//! ulteriore `RawConfig` che si sovrappone all'`.env`.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::cli::Cli;
use crate::error::StepError;
use crate::secret::Secret;

// --- Default e costanti (allineati al Bash) ---------------------------------

/// `ODOO_HOME` è una costante architetturale: non sovrascrivibile.
pub const ODOO_HOME: &str = "/opt/odoo";
const DEFAULT_VERSION: &str = "18.0";
const DEFAULT_ODOO_USER: &str = "odoo";
const DEFAULT_PORT: &str = "8069";
const DEFAULT_DB_NAME: &str = "odoo";
const DEFAULT_ADMIN_PASSWD: &str = "admin";

// --- Errori di configurazione ------------------------------------------------

/// Errore di risoluzione/validazione della configurazione.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("versione Odoo non valida: '{0}'. Valori ammessi: 16|17|18|19 oppure 16.0..19.0")]
    InvalidVersion(String),

    #[error(
        "{field} non valido: '{value}'. Usa solo lettere, numeri, punto, trattino o underscore, \
         e comincia con una lettera, una cifra o un underscore (un nome che inizia con '-' \
         verrebbe scambiato per un'opzione dai comandi di sistema)"
    )]
    InvalidIdentifier { field: &'static str, value: String },

    #[error("porta Odoo non valida: '{0}'. Inserisci un numero tra 1 e 65535")]
    InvalidPort(String),

    #[error("install dir non valida: '{0}'. Inserisci un percorso assoluto")]
    InstallDirNotAbsolute(PathBuf),

    #[error("install dir non valida: '{0}'. Deve essere sotto '{1}'")]
    InstallDirOutOfScope(PathBuf, String),

    #[error("la password admin Odoo non può essere vuota")]
    EmptyPassword,

    #[error(
        "admin_passwd='admin' richiede una conferma esplicita interattiva. \
         Imposta una password diversa oppure riesegui in modalità interattiva"
    )]
    InsecureAdminNonInteractive,

    #[error("config file non trovato: {0}")]
    ConfigFileNotFound(PathBuf),

    #[error("errore di lettura del config file '{path}': {source}")]
    ConfigFileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// --- RawConfig: valori grezzi da una singola sorgente ------------------------

/// Valori grezzi (non validati) provenienti da una sorgente: CLI, `.env`, o
/// prompt interattivi. Ogni campo è `Option`: `None` = non fornito da questa
/// sorgente. `admin_passwd` è tenuta in chiaro qui perché va parsata, ma la
/// struct **non** implementa `Debug` di proposito, per non rischiare leak nei log.
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
    pub enable_ssl: Option<bool>,
}

impl RawConfig {
    /// Estrae i valori grezzi dagli argomenti CLI.
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
            // `--with-nginx` non passato → None (cade su .env/default); non c'è
            // negazione da CLI, esattamente come nel Bash.
            with_nginx: if cli.with_nginx { Some(true) } else { None },
            server_name: cli.server_name.clone(),
            enable_ssl: if cli.enable_ssl { Some(true) } else { None },
        }
    }
}

/// Parser `.env` **dichiarativo**: legge `KEY=VALUE` riga per riga.
///
/// Ignora righe vuote e commenti (`#`), tollera un prefisso `export`, rimuove
/// eventuali apici attorno al valore. **Non esegue nulla**: nessuna espansione
/// di comandi, nessun `eval`. Un valore come `$(rm -rf /)` viene trattato come
/// stringa letterale. Le chiavi sconosciute producono un warning e vengono
/// ignorate (non è un errore).
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
        // Tollera "export KEY=VALUE".
        let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);

        let Some((key, value)) = line.split_once('=') else {
            warn!(line = lineno + 1, "riga .env ignorata (manca '=')");
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
            "NGINX_ENABLE_SSL" => raw.enable_ssl = Some(parse_bool(&value)),
            "ODOO_HOME" => warn!("ODOO_HOME è una costante fissa, chiave .env ignorata"),
            other => warn!(key = other, "chiave .env sconosciuta ignorata"),
        }
    }

    Ok(raw)
}

/// Rimuove una sola coppia di apici (singoli o doppi) che avvolgano l'intero
/// valore. Nessuna interpretazione ulteriore.
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

/// Interpreta un booleano testuale (`true`/`1`/`yes`/`on`, case-insensitive).
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

// --- Validatori (puri, testabili) -------------------------------------------

/// Normalizza la versione Odoo. Ritorna `(full, short)`, es. `("18.0", "18")`.
pub fn normalize_version(value: &str) -> Result<(String, String), ConfigError> {
    let full = match value {
        "16" | "17" | "18" | "19" => format!("{value}.0"),
        "16.0" | "17.0" | "18.0" | "19.0" => value.to_string(),
        _ => return Err(ConfigError::InvalidVersion(value.to_string())),
    };
    let short = full.split('.').next().unwrap_or(full.as_str()).to_string();
    Ok((full, short))
}

/// `true` se `value` è un identifier valido (`^[A-Za-z0-9_][A-Za-z0-9._-]*$`).
///
/// Il **primo** carattere deve essere alfanumerico o `_`. Il vincolo non è
/// estetico: `db_name`/`db_user`/`odoo_user` finiscono come argomenti
/// posizionali di `createdb`/`dropdb`/`useradd`, e un nome come `-foo` o
/// `--help` verrebbe interpretato dal comando come **flag** invece che come
/// operando (argument injection). Un identifier che inizia con `-` (o con `.`,
/// che darebbe un file/DB "nascosto") non è mai legittimo qui.
///
/// È la porta a monte; la rete a valle è il `--` prima dei posizionali in
/// [`crate::system_ops::argv`]. Servono entrambe.
fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Valida un identifier, allegando il nome del campo all'errore.
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

/// Valida una porta (intero 1..=65535).
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

/// Risolve e valida la install dir: default derivato, assoluta e sotto `home`.
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
    // `starts_with` su Path è component-based: `/opt/odoofoo` NON inizia con
    // `/opt/odoo` (evita il bug del prefisso di stringa del Bash). Copre anche
    // il caso dir == home.
    if !dir.starts_with(home) {
        return Err(ConfigError::InstallDirOutOfScope(
            dir,
            home.to_string_lossy().into_owned(),
        ));
    }
    Ok(dir)
}

/// Esito del controllo sulla password admin.
#[derive(Debug, PartialEq, Eq)]
pub enum AdminConfirm {
    /// Password diversa da 'admin': nessuna conferma richiesta.
    NotNeeded,
    /// Password 'admin' in modalità interattiva: serve conferma esplicita y/N.
    ConfirmNeeded,
}

/// Applica la regola sulla password admin (pura, senza I/O).
///
/// - vuota → errore;
/// - diversa da 'admin' → ok, nessuna conferma;
/// - 'admin' e **non** interattivo → errore (hard-stop: non si può confermare
///   senza TTY);
/// - 'admin' e interattivo → richiede conferma esplicita al chiamante.
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

// --- Config risolta ----------------------------------------------------------

/// Configurazione risolta e validata, pronta a costruire il
/// [`crate::context::Context`].
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub version: String,
    pub version_short: String,
    pub odoo_user: String,
    pub db_user: String,
    /// Password del ruolo PostgreSQL; vuota = autenticazione peer.
    pub db_password: Secret,
    pub odoo_home: PathBuf,
    pub port: u16,
    pub db_name: String,
    pub install_dir: PathBuf,
    pub admin_passwd: Secret,
    /// Logfile Odoo; `None` = log su journal/stdout (default).
    pub odoo_logfile: Option<PathBuf>,
    pub with_nginx: bool,
    pub nginx_server_name: String,
    pub nginx_enable_ssl: bool,
}

/// Sceglie il primo valore presente nella cascata (priorità da sinistra).
fn pick(cli: &Option<String>, prompted: &Option<String>, env: &Option<String>) -> Option<String> {
    cli.clone()
        .or_else(|| prompted.clone())
        .or_else(|| env.clone())
}

impl ResolvedConfig {
    /// Risolve la config combinando le sorgenti secondo la cascata di priorità:
    /// **CLI → prompt interattivo → `.env` → default**.
    ///
    /// `prompted` contiene i valori raccolti interattivamente (vuoto in modalità
    /// non-interattiva). La funzione è pura: non esegue prompt né tocca il disco.
    /// La conferma interattiva della password 'admin' resta a carico del
    /// chiamante (vedi [`check_admin_password`]); qui si applica solo l'hard-stop
    /// non-interattivo.
    pub fn resolve(
        cli: &RawConfig,
        env: &RawConfig,
        prompted: &RawConfig,
        interactive: bool,
    ) -> Result<Self, ConfigError> {
        let home = PathBuf::from(ODOO_HOME);

        // Versione (+ short).
        let version_raw = pick(&cli.version, &prompted.version, &env.version)
            .unwrap_or_else(|| DEFAULT_VERSION.to_string());
        let (version, version_short) = normalize_version(&version_raw)?;

        // Utente OS.
        let odoo_user_raw = pick(&cli.odoo_user, &prompted.odoo_user, &env.odoo_user)
            .unwrap_or_else(|| DEFAULT_ODOO_USER.to_string());
        let odoo_user = validate_identifier(&odoo_user_raw, "utente Odoo")?;

        // Nome DB.
        let db_name_raw = pick(&cli.db_name, &prompted.db_name, &env.db_name)
            .unwrap_or_else(|| DEFAULT_DB_NAME.to_string());
        let db_name = validate_identifier(&db_name_raw, "nome database")?;

        // Porta.
        let port_raw =
            pick(&cli.port, &prompted.port, &env.port).unwrap_or_else(|| DEFAULT_PORT.to_string());
        let port = validate_port(&port_raw)?;

        // Utente DB: segue odoo_user se non disaccoppiato esplicitamente.
        let db_user = resolve_db_user(cli.db_user.as_deref(), env.db_user.as_deref(), &odoo_user)?;

        // Password del ruolo DB: vuota/assente → peer auth (Secret vuoto).
        let db_password = Secret::new(
            pick(&cli.db_password, &prompted.db_password, &env.db_password).unwrap_or_default(),
        );

        // Install dir (default derivato, assoluta, sotto home).
        let install_dir_raw = pick(&cli.install_dir, &prompted.install_dir, &env.install_dir);
        let install_dir = resolve_install_dir(install_dir_raw.as_deref(), &home, &version_short)?;

        // Password admin.
        let admin_raw = pick(&cli.admin_passwd, &prompted.admin_passwd, &env.admin_passwd)
            .unwrap_or_else(|| DEFAULT_ADMIN_PASSWD.to_string());
        // Hard-stop non-interattivo; la conferma interattiva è del chiamante.
        check_admin_password(&admin_raw, interactive)?;

        // Logfile: opzionale; una stringa vuota equivale a "disabilitato".
        let odoo_logfile = pick(&cli.logfile, &prompted.logfile, &env.logfile)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        // Nginx.
        let with_nginx = cli
            .with_nginx
            .or(prompted.with_nginx)
            .or(env.with_nginx)
            .unwrap_or(false);
        let nginx_server_name = pick(&cli.server_name, &prompted.server_name, &env.server_name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "_".to_string());
        let nginx_enable_ssl = cli
            .enable_ssl
            .or(prompted.enable_ssl)
            .or(env.enable_ssl)
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
            nginx_enable_ssl,
        })
    }
}

/// Applica la regola "db_user segue odoo_user".
///
/// `db_user` = `odoo_user` se non è stato fornito, **oppure** se non è stato
/// passato da CLI e vale il default `odoo`. Solo un valore esplicito da CLI, o
/// un valore da `.env` diverso dal default, lo disaccoppia (semantica di
/// `validate_selected_inputs` nel Bash).
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
                validate_identifier(value, "utente database")
            }
        }
    }
}

// Rende `StepError` costruibile da un errore di config al confine col motore,
// senza accoppiare i due moduli (usato nelle fasi successive).
impl From<ConfigError> for StepError {
    fn from(err: ConfigError) -> Self {
        StepError::Precondition(err.to_string())
    }
}
