//! config resolution: the cascade, the subtle rules, the declarative `.env`
//! parser, validation and the password hard stop.

use std::io::Write;
use std::path::PathBuf;

use invok::config::{
    self, check_admin_password, normalize_version, parse_env_file, validate_port, AdminConfirm,
    ConfigError, RawConfig, ResolvedConfig,
};

/// a baseline CLI config with a non-default password, so non-interactive
/// resolution does not trip the hard stop.
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

// --- the cascade ------------------------------------------------------------

#[test]
fn cascade_cli_beats_env_beats_default() {
    // the CLI beats the env file.
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

    // the env file beats the default.
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

    // the default when neither supplies anything.
    let r = resolve(&cli_base(), &RawConfig::default()).expect("resolve ok");
    assert_eq!(r.version, "18.0");
    assert_eq!(r.port, 8069);
    assert_eq!(r.db_name, "odoo");
}

// --- db_user follows odoo_user ----------------------------------------------

#[test]
fn db_user_follows_odoo_user_unless_explicit() {
    // absent, with a custom OS user: they stay coupled.
    let cli = RawConfig {
        odoo_user: Some("custom".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.odoo_user, "custom");
    assert_eq!(r.db_user, "custom");

    // explicit: decoupled.
    let cli = RawConfig {
        odoo_user: Some("custom".to_string()),
        db_user: Some("dbx".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.db_user, "dbx");

    // from the env file and different from the default: decoupled without the
    // CLI.
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

    // from the env file but equal to the default: still coupled.
    let env = RawConfig {
        db_user: Some("odoo".to_string()),
        ..Default::default()
    };
    let r = resolve(&cli, &env).expect("resolve");
    assert_eq!(r.db_user, "custom");
}

// --- the .env parser: declarative, never evaluated --------------------------

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
    // an unknown key and a malformed line yield no field and no panic.
}

#[test]
fn env_parser_does_not_execute_command_substitution() {
    let dir = tempfile::tempdir().expect("tempdir");
    // a sentinel file: an evaluating parser would remove it.
    let sentinel = dir.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"vivo").expect("write sentinel");

    let path = dir.path().join("danger.env");
    let mut f = std::fs::File::create(&path).expect("create");
    // a "dangerous" value must be treated as a literal string.
    writeln!(f, "ODOO_ADMIN_PASSWD=$(rm -f {})", sentinel.display()).unwrap();
    writeln!(f, "ODOO_USER=`whoami`").unwrap();
    drop(f);

    let raw = parse_env_file(&path).expect("parse");

    // captured verbatim, not executed.
    assert_eq!(
        raw.admin_passwd.as_deref(),
        Some(format!("$(rm -f {})", sentinel.display()).as_str())
    );
    assert_eq!(raw.odoo_user.as_deref(), Some("`whoami`"));
    // and above all the sentinel still exists.
    assert!(
        sentinel.exists(),
        "il parser .env NON deve eseguire comandi"
    );
}

// --- validation -------------------------------------------------------------

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

    // through the cascade from the env file too.
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

    // a relative path is rejected.
    let cli = RawConfig {
        install_dir: Some("opt/odoo/x".to_string()),
        ..cli_base()
    };
    let err = resolve(&cli, &RawConfig::default()).expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InstallDirNotAbsolute(_)));

    // a valid path under the home passes.
    let cli = RawConfig {
        install_dir: Some("/opt/odoo/custom".to_string()),
        ..cli_base()
    };
    let r = resolve(&cli, &RawConfig::default()).expect("resolve");
    assert_eq!(r.install_dir, PathBuf::from("/opt/odoo/custom"));

    // the default derived from the version.
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

#[test]
fn identifier_rejects_leading_dash_and_dot() {
    // a name starting with `-` would be read as a **flag** by the commands that
    // take it positionally (argument injection), so it is rejected upstream.
    // same for a leading dot, never legitimate here.
    for bad in ["-foo", "--help", "-", ".bar", ".", "--", ""] {
        assert!(
            matches!(
                config::validate_identifier(bad, "db_name"),
                Err(ConfigError::InvalidIdentifier { .. })
            ),
            "'{bad}' deve essere rifiutato"
        );
    }
}

#[test]
fn identifier_accepts_internal_dash_and_dot() {
    // the constraint is on the **first** character only: internal dashes and
    // dots are legitimate.
    for good in ["foo", "foo_bar", "foo-bar", "foo.bar", "_foo", "0foo", "F"] {
        assert_eq!(
            config::validate_identifier(good, "db_name").expect("deve essere valido"),
            good
        );
    }
}

// --- the master password ----------------------------------------------------

#[test]
fn admin_default_non_interactive_is_hard_stop() {
    // no password supplied, so the weak default, non-interactively: error.
    let err = ResolvedConfig::resolve(
        &RawConfig::default(),
        &RawConfig::default(),
        &RawConfig::default(),
        false,
    )
    .expect_err("deve fallire");
    assert!(matches!(err, ConfigError::InsecureAdminNonInteractive));

    // the pure check too.
    assert!(matches!(
        check_admin_password("admin", false),
        Err(ConfigError::InsecureAdminNonInteractive)
    ));
    // interactively it asks for confirmation, which is not an error.
    assert_eq!(
        check_admin_password("admin", true).expect("ok"),
        AdminConfirm::ConfirmNeeded
    );
    // a different password needs no confirmation.
    assert_eq!(
        check_admin_password("s3cret", false).expect("ok"),
        AdminConfirm::NotNeeded
    );
    // empty is an error.
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

// --- version normalisation --------------------------------------------------

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
    // the field is public and used; this silences a pointless warning.
    let _ = config::ODOO_HOME;
}

/// A-V3-6: the historical `.env` key stays recognised. it lives in
/// configuration files already handed to customers, and ignoring it silently
/// would leave a port closed that the user believes open.
#[test]
fn the_historical_env_key_for_the_https_port_still_works() {
    let dir = tempfile::tempdir().expect("tempdir");

    let storico = dir.path().join("storico.env");
    std::fs::write(&storico, "NGINX_ENABLE_SSL=true\n").expect("write");
    let raw = config::parse_env_file(&storico).expect("parse");
    assert_eq!(raw.open_https_port, Some(true));

    let nuovo = dir.path().join("nuovo.env");
    std::fs::write(&nuovo, "NGINX_OPEN_HTTPS_PORT=true\n").expect("write");
    let raw = config::parse_env_file(&nuovo).expect("parse");
    assert_eq!(raw.open_https_port, Some(true));
}
