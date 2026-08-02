//! Coerenza della configurazione usata dalla CI di integrazione (R5).
//!
//! `configs/ci.env`, `scripts/ci/integration-test.sh` e il parser `.env` devono
//! dire la stessa cosa. Se divergono, il sintomo arriva dopo **quaranta minuti**
//! di job — o peggio, non arriva affatto: una chiave scritta male non è un
//! errore, è un warning, e l'installer prosegue con i default. Il test di
//! integrazione andrebbe a verificare un database `odoo` che nessuno ha creato,
//! e passerebbe o fallirebbe per il motivo sbagliato.
//!
//! Questi test girano nella CI veloce (`test.yml`), su mock, in millisecondi.

use std::path::Path;

use odoo_installer::config::{self, ResolvedConfig};

const CI_ENV: &str = "configs/ci.env";
const CI_NGINX_ENV: &str = "configs/ci-nginx.env";
const CI_SCRIPT: &str = "scripts/ci/integration-test.sh";

/// Le chiavi che `config::parse_env_file` riconosce.
///
/// Duplicate qui di proposito, come **guardia**: la fonte di verità resta il
/// `match` in `config.rs`. Se qualcuno aggiunge una chiave a `ci.env` che il
/// parser ignorerebbe in silenzio, questo elenco la fa emergere subito.
const KNOWN_KEYS: &[&str] = &[
    "ODOO_VERSION",
    "ODOO_USER",
    "DB_USER",
    "DB_PASSWORD",
    "ODOO_PORT",
    "DB_NAME",
    "ODOO_INSTALL_DIR",
    "ODOO_ADMIN_PASSWD",
    "ODOO_LOGFILE",
    "WITH_NGINX",
    "NGINX_SERVER_NAME",
    "NGINX_OPEN_HTTPS_PORT",
    // Nome storico, ancora riconosciuto: vive nei `.env` dei clienti (A-V3-6).
    "NGINX_ENABLE_SSL",
];

fn resolve_ci_env() -> ResolvedConfig {
    let raw = config::parse_env_file(Path::new(CI_ENV)).expect("configs/ci.env deve esistere");
    let empty = config::RawConfig::default();
    // `interactive = false`: è come gira in CI, senza TTY. Il flag conta —
    // in non-interattivo la password debole 'admin' è un hard-stop.
    ResolvedConfig::resolve(&empty, &raw, &empty, false)
        .expect("configs/ci.env deve risolvere senza intervento interattivo")
}

#[test]
fn every_key_in_ci_env_is_understood_by_the_parser() {
    for file in [CI_ENV, CI_NGINX_ENV] {
        assert_keys_are_known(file);
    }
}

fn assert_keys_are_known(file: &str) {
    let content = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("{file} deve esistere"));
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line
            .strip_prefix("export ")
            .unwrap_or(line)
            .split_once('=')
            .map(|(k, _)| k.trim())
            .unwrap_or_else(|| panic!("riga {} senza '=': {line}", lineno + 1));
        assert!(
            KNOWN_KEYS.contains(&key),
            "{file} riga {}: la chiave '{key}' non è riconosciuta dal parser .env. \
             Verrebbe ignorata con un warning e l'installer userebbe il default, \
             falsando il test di integrazione.",
            lineno + 1
        );
    }
}

/// B-V3-7: la config con nginx differisce da quella base **solo** per nginx.
///
/// Se divergessero anche su altro, un fallimento che compare solo nel job nginx
/// non sarebbe più attribuibile a nginx — e il valore di avere due job
/// identici-tranne-uno starebbe tutto lì.
#[test]
fn the_nginx_ci_config_differs_only_by_nginx() {
    let base = config::parse_env_file(Path::new(CI_ENV)).expect("ci.env");
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");

    assert_eq!(base.version, con_nginx.version);
    assert_eq!(base.odoo_user, con_nginx.odoo_user);
    assert_eq!(base.db_name, con_nginx.db_name);
    assert_eq!(base.db_user, con_nginx.db_user);
    assert_eq!(base.port, con_nginx.port);
    assert_eq!(base.admin_passwd, con_nginx.admin_passwd);
    assert_eq!(base.logfile, con_nginx.logfile);

    assert_eq!(
        base.with_nginx,
        Some(false),
        "la config base resta senza nginx"
    );
    assert_eq!(
        con_nginx.with_nginx,
        Some(true),
        "è l'unica ragione per cui questo file esiste"
    );
}

/// Il flag che apre la 443 resta **fuori** dalla config di CI, e non per caso:
/// su un runner `ufw` è installato ma inattivo, quindi lo step uscirebbe subito
/// senza aggiungere copertura — e il flag comunque non tocca il vhost (A-V3-6).
#[test]
fn the_nginx_ci_config_does_not_ask_for_the_https_port() {
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");
    assert_eq!(con_nginx.open_https_port, None);
}

#[test]
fn ci_env_resolves_to_what_the_integration_script_expects() {
    let cfg = resolve_ci_env();

    // Il nome del database è il perno del test: NON deve essere il default.
    // Se il rollback ricavasse i nomi dai default invece che dalla config
    // persistita (regressione A-R4-1), cercherebbe 'odoo' e lascerebbe
    // 'citest' sul sistema — e il test di pulizia lo vedrebbe.
    assert_eq!(cfg.db_name, "citest");
    assert_ne!(
        cfg.db_name, "odoo",
        "il db_name della CI deve differire dal default, altrimenti il test non \
         distingue 'il rollback ha usato la config persistita' da 'ha indovinato'"
    );

    assert_eq!(cfg.version, "18.0");
    assert_eq!(cfg.version_short, "18");
    assert_eq!(cfg.odoo_user, "odoo");
    assert_eq!(cfg.db_user, "odoo");
    assert_eq!(cfg.port, 8069);
    assert!(!cfg.with_nginx, "la sonda di CI non configura Nginx");
    assert!(
        cfg.odoo_logfile.is_none(),
        "ODOO_LOGFILE vuoto = log su journal: nessuna log dir da verificare"
    );
    assert!(
        cfg.db_password.is_empty(),
        "password vuota = autenticazione peer, il percorso che vogliamo esercitare"
    );
}

#[test]
fn the_ci_admin_password_is_not_the_weak_default() {
    // In non-interattivo `resolve` rifiuta la password 'admin'
    // (InsecureAdminNonInteractive): con quel valore la CI non partirebbe
    // nemmeno. `resolve_ci_env` lo prova già col suo `expect`; qui si dice
    // perché, così chi tocca il file sa cosa sta toccando.
    let cfg = resolve_ci_env();
    assert_ne!(
        cfg.admin_passwd.expose(),
        "admin",
        "una password 'admin' fa fallire l'installazione non interattiva prima \
         di qualsiasi step"
    );
}

#[test]
fn the_integration_script_and_ci_env_agree_on_the_artifacts() {
    // I default dello script devono combaciare coi valori del .env: lo script
    // verifica *per nome* il database, l'utente e la porta, e un disallineamento
    // renderebbe le asserzioni vacue (cercherebbero artefatti che nessuno ha
    // creato) invece di farle fallire.
    let script = std::fs::read_to_string(CI_SCRIPT).expect("scripts/ci/integration-test.sh");
    let cfg = resolve_ci_env();

    for (var, expected) in [
        ("DB_NAME", cfg.db_name.clone()),
        ("DB_ROLE", cfg.db_user.clone()),
        ("OS_USER", cfg.odoo_user.clone()),
        ("PORT", cfg.port.to_string()),
        ("VER_SHORT", cfg.version_short.clone()),
    ] {
        let needle = format!("{var}:-{expected}}}");
        assert!(
            script.contains(&needle),
            "lo script di integrazione deve avere `${{{var}:-{expected}}}` per \
             combaciare con configs/ci.env (atteso il frammento `{needle}`)"
        );
    }
}

#[test]
fn the_integration_script_is_executable_and_syntactically_valid() {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(CI_SCRIPT).expect("lo script deve esistere");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "lo script di integrazione deve avere il bit di esecuzione"
    );

    // `bash -n` non esegue nulla: analizza soltanto la sintassi. Uno script
    // rotto verrebbe scoperto altrimenti solo a metà di un job da 40 minuti,
    // dopo aver già installato mezzo sistema.
    let status = std::process::Command::new("bash")
        .args(["-n", CI_SCRIPT])
        .status()
        .expect("bash deve essere disponibile");
    assert!(status.success(), "sintassi non valida in {CI_SCRIPT}");
}
