//! Test della risoluzione config (Fase 1): cascata, regole sottili, parser
//! `.env` dichiarativo (no-eval), validazione, hard-stop password.

use std::io::Write;
use std::path::PathBuf;

use odoo_installer::config::{
    self, check_admin_password, normalize_version, parse_env_file, validate_port, AdminConfirm,
    ConfigError, RawConfig, ResolvedConfig,
};

/// RawConfig CLI di base con una password non-default, così la risoluzione
/// non-interattiva non inciampa nell'hard-stop su 'admin'.
fn cli_base() -> RawConfig {
    RawConfig {
        admin_passwd: Some("s3cret".to_string()),
        ..Default::default()
    }
}

fn resolve(cli: &RawConfig, env: &RawConfig) -> Result<ResolvedConfig, ConfigError> {
    ResolvedConfig::resolve(
        cli,
        env,
        &RawConfig::default(),
        /* interactive */ false,
    )
}

// --- Cascata -----------------------------------------------------------------

#[test]
fn cascade_cli_beats_env_beats_default() {
    // CLI batte env.
    let cli = RawConfig {
        version: Some("17".to_string()),
        port: Some("9000".to_string()),
        db_name: Some("clidb".to_string()),
        ..cli_base()
    };
    let env = RawConfig {
        version: Some("16".to_string()),
        port: Some("8888".to_string()),
        db_name: Some("envdb".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli, &env).expect("resolve ok");
    assert_eq!(r.version, "17.0");
    assert_eq!(r.port, 9000);
    assert_eq!(r.db_name, "clidb");

    // Env batte default (CLI assente su questi campi).
    let env_only = RawConfig {
        version: Some("19".to_string()),
        port: Some("7000".to_string()),
        db_name: Some("only".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli_base(), &env_only).expect("resolve ok");
    assert_eq!(r.version, "19.0");
    assert_eq!(r.port, 7000);
    assert_eq!(r.db_name, "only");

    // Default quando né CLI né env forniscono nulla.
    let r = resolve(&cli_base(), &RawConfig::default()).expect("resolve ok");
    assert_eq!(r.version, "18.0");
    assert_eq!(r.port, 8069);
    assert_eq!(r.db_name, "odoo");
}

// --- db_user segue odoo_user -------------------------------------------------

#[test]
fn db_user_follows_odoo_user_unless_explicit() {
    // --db-user assente + odoo_user custom → db_user = odoo_user.
    let cli = RawConfig {
        odoo_user: Some("custom".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.odoo_user, "custom");
    assert_eq!(r.db_user, "custom");

    // --db-user esplicito → resta disaccoppiato.
    let cli = RawConfig {
        odoo_user: Some("custom".to_string()),
        db_user: Some("dbx".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.db_user, "dbx");

    // DB_USER da .env, diverso dal default → disaccoppia anche senza CLI.
    let cli = RawConfig {
        odoo_user: Some("custom".to_string()),
        ..cli_base()
    };
    let env = RawConfig {
        db_user: Some("envdb".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli, &env).expect("resolve");
    assert_eq!(r.db_user, "envdb");

    // DB_USER da .env uguale al default 'odoo' e nessun CLI → segue odoo_user.
    let env = RawConfig {
        db_user: Some("odoo".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli, &env).expect("resolve");
    assert_eq!(r.db_user, "custom");
}

// --- Parser .env: dichiarativo, no-eval --------------------------------------

#[test]
fn env_parser_reads_pairs_ignores_comments_and_blanks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cfg.env");
    let mut f = std::fs::File::create(&path).expect("create");
    writeln!(f, "# commento").unwrap();
    writeln!(f).unwrap();
    writeln!(f, "ODOO_VERSION=17").unwrap();
    writeln!(f, "export ODOO_USER=\"quoted\"").unwrap();
    writeln!(f, "WITH_NGINX=true").unwrap();
    writeln!(f, "UNKNOWN_KEY=whatever").unwrap();
    writeln!(f, "riga senza uguale").unwrap();
    drop(f);

    let raw = parse_env_file(&path).expect("parse");
    assert_eq!(raw.version.as_deref(), Some("17"));
    assert_eq!(raw.odoo_user.as_deref(), Some("quoted")); // apici rimossi
    assert_eq!(raw.with_nginx, Some(true));
    // chiave sconosciuta e riga malformata non producono campi né panico.
}

#[test]
fn env_parser_does_not_execute_command_substitution() {
    let dir = tempfile::tempdir().expect("tempdir");
    // File-sentinella: se il parser eseguisse il valore, verrebbe rimosso.
    let sentinel = dir.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"vivo").expect("write sentinel");

    let path = dir.path().join("danger.env");
    let mut f = std::fs::File::create(&path).expect("create");
    // Valore "pericoloso": deve essere trattato come stringa letterale.
    writeln!(f, "ODOO_ADMIN_PASSWD=$(rm -f {})", sentinel.display()).unwrap();
    writeln!(f, "ODOO_USER=`whoami`").unwrap();
    drop(f);

    let raw = parse_env_file(&path).expect("parse");

    // Il valore è catturato alla lettera, non eseguito.
    assert_eq!(
        raw.admin_passwd.as_deref(),
        Some(format!("$(rm -f {})", sentinel.display()).as_str())
    );
    assert_eq!(raw.odoo_user.as_deref(), Some("`whoami`"));
    // E soprattutto: il file-sentinella esiste ancora (niente esecuzione).
    assert!(
        sentinel.exists(),
        "il parser .env NON deve eseguire comandi"
    );
}

// --- Validazione -------------------------------------------------------------

#[test]
fn invalid_version_is_typed_error() {
    let cli = RawConfig {
        version: Some("20".to_string()),
        ..cli_base()
    };
    let err = resolve(&cli, &RawConfig::default()).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InvalidVersion(_)));
}

#[test]
fn port_out_of_range_is_typed_error() {
    assert!(matches!(
        validate_port("0"),
        Err(ConfigError::InvalidPort(_))
    ));
    assert!(matches!(
        validate_port("70000"),
        Err(ConfigError::InvalidPort(_))
    ));
    assert!(matches!(
        validate_port("abc"),
        Err(ConfigError::InvalidPort(_))
    ));
    assert_eq!(validate_port("8069").expect("ok"), 8069);

    // Anche via cascata da .env.
    let env = RawConfig {
        port: Some("70000".to_string()),
        ..Default::default()
    };
    let err = resolve(&cli_base(), &env).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InvalidPort(_)));
}

#[test]
fn install_dir_out_of_scope_is_typed_error() {
    let cli = RawConfig {
        install_dir: Some("/etc/odoo".to_string()),
        ..cli_base()
    };
    let err = resolve(&cli, &RawConfig::default()).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InstallDirOutOfScope(_, _)));

    // Un path relativo → NotAbsolute.
    let cli = RawConfig {
        install_dir: Some("opt/odoo/x".to_string()),
        ..cli_base()
    };
    let err = resolve(&cli, &RawConfig::default()).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InstallDirNotAbsolute(_)));

    // Path valido sotto /opt/odoo → ok.
    let cli = RawConfig {
        install_dir: Some("/opt/odoo/custom".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.install_dir, PathBuf::from("/opt/odoo/custom"));

    // Default derivato dalla versione.
    let cli = RawConfig {
        version: Some("17".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.install_dir, PathBuf::from("/opt/odoo/odoo17"));
}

#[test]
fn empty_db_name_is_typed_error() {
    let env = RawConfig {
        db_name: Some(String::new()),
        ..Default::default()
    };
    let err = resolve(&cli_base(), &env).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InvalidIdentifier { .. }));
}

// --- Password admin ----------------------------------------------------------

#[test]
fn admin_default_non_interactive_is_hard_stop() {
    // Nessuna password fornita → default 'admin', non interattivo → errore.
    let err = ResolvedConfig::resolve(
        &RawConfig::default(),
        &RawConfig::default(),
        &RawConfig::default(),
        false,
    )
    .expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InsecureAdminNonInteractive));

    // Anche il controllo puro.
    assert!(matches!(
        check_admin_password("admin", false),
        Err(ConfigError::InsecureAdminNonInteractive)
    ));
    // Interattivo → richiede conferma (non è errore).
    assert_eq!(
        check_admin_password("admin", true).expect("ok"),
        AdminConfirm::ConfirmNeeded
    );
    // Password diversa → nessuna conferma.
    assert_eq!(
        check_admin_password("s3cret", false).expect("ok"),
        AdminConfirm::NotNeeded
    );
    // Vuota → errore.
    assert!(matches!(
        check_admin_password("", true),
        Err(ConfigError::EmptyPassword)
    ));
}

#[test]
fn custom_password_non_interactive_is_ok() {
    let cli = RawConfig {
        admin_passwd: Some("robusta!".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.admin_passwd.expose(), "robusta!");
}

// --- Normalizzazione versione ------------------------------------------------

#[test]
fn version_normalization() {
    assert_eq!(
        normalize_version("18").expect("ok"),
        ("18.0".to_string(), "18".to_string())
    );
    assert_eq!(
        normalize_version("19.0").expect("ok"),
        ("19.0".to_string(), "19".to_string())
    );
    assert!(matches!(
        normalize_version("20"),
        Err(ConfigError::InvalidVersion(_))
    ));
    // Il campo `config` è pubblico e usato: silenzia eventuali warning inutili.
    let _ = config::ODOO_HOME;
}
