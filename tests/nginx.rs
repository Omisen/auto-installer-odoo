//! Test della fase Nginx (Fase 9): gating, protezione default site, delta ufw.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::nginx_enable_site::NginxEnableSite;
use odoo_installer::steps::nginx_firewall::NginxFirewall;
use odoo_installer::steps::nginx_install::NginxInstall;
use odoo_installer::steps::nginx_reload::NginxReload;
use odoo_installer::steps::nginx_write_config::{render_vhost, validate_vhost, NginxWriteConfig};

fn ctx(with_nginx: bool, ssl: bool) -> Context {
    Context {
        with_nginx,
        nginx_enable_ssl: ssl,
        nginx_server_name: "_".to_string(),
        odoo_version_short: "18".to_string(),
        port: 8069,
        dry_run: false,
        ..Default::default()
    }
}

fn rules(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn default_site() -> PathBuf {
    PathBuf::from("/etc/nginx/sites-enabled/default")
}

// --- Gating: --with-nginx assente → tutto inerte ----------------------------

#[test]
fn gating_all_steps_are_noop_without_nginx() {
    let c = ctx(/* with_nginx */ false, false);

    macro_rules! check_noop {
        ($step:expr) => {{
            let (mock, log) = MockSystemOps::new(MockConfig::default());
            let mut step = $step(Box::new(mock));
            step.snapshot(&c).expect("snapshot");
            step.run(&c).expect("run");
            step.undo(&c).expect("undo");
            assert!(ops_of(&log).is_empty(), "step inerte senza --with-nginx");
        }};
    }

    check_noop!(NginxInstall::with_ops);
    check_noop!(NginxWriteConfig::with_ops);
    check_noop!(NginxEnableSite::with_ops);
    check_noop!(NginxFirewall::with_ops);
    check_noop!(NginxReload::with_ops);
}

// --- Default site: la protezione della fase ---------------------------------

#[test]
fn default_site_removed_then_restored() {
    // IL test protettivo: il default site esisteva → run lo rimuove → undo lo
    // ripristina (config Nginx del cliente riportata com'era).
    let cfg = MockConfig {
        default_site_exists: true,
        our_link_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // run rimuove il default site.
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveSymlink(p) if *p == default_site())));
    // undo lo RIPRISTINA (ricrea il symlink default).
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::CreateSymlink { link, .. } if *link == default_site())),
        "undo deve ripristinare il default site: {ops:?}"
    );
}

#[test]
fn absent_default_site_is_not_invented() {
    // Non esisteva → run non lo tocca → undo non lo crea.
    let cfg = MockConfig {
        default_site_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(!ops
        .iter()
        .any(|o| matches!(o, Op::RemoveSymlink(p) if *p == default_site())));
    assert!(
        !ops.iter()
            .any(|o| matches!(o, Op::CreateSymlink { link, .. } if *link == default_site())),
        "non inventiamo un default site che non c'era"
    );
}

// --- Firewall: pattern delta -------------------------------------------------

#[test]
fn firewall_undo_removes_only_the_delta() {
    // 80 già presente (non nel delta), 443 aggiunta da noi → undo rimuove solo 443.
    let cfg = MockConfig {
        ufw_available: true,
        ufw_active: true,
        existing_ufw_rules: rules(&["80/tcp"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxFirewall::with_ops(Box::new(mock));
    let c = ctx(true, /* ssl */ true); // desidera 80 + 443

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::UfwAllow("443/tcp".to_string())),
        "apre solo il delta (443)"
    );
    assert!(
        !ops.contains(&Op::UfwAllow("80/tcp".to_string())),
        "80 c'era già: non riaperta"
    );
    assert!(
        ops.contains(&Op::UfwDelete("443/tcp".to_string())),
        "undo rimuove il delta"
    );
    assert!(
        !ops.contains(&Op::UfwDelete("80/tcp".to_string())),
        "MAI rimuovere una regola preesistente del cliente"
    );
}

#[test]
fn firewall_noop_when_ufw_inactive() {
    let cfg = MockConfig {
        ufw_available: true,
        ufw_active: false, // presente ma non attivo
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxFirewall::with_ops(Box::new(mock));
    let c = ctx(true, true);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "ufw inattivo → nessuna azione firewall"
    );
}

// --- Reload / Install --------------------------------------------------------

#[test]
fn reload_fails_on_invalid_config() {
    let cfg = MockConfig {
        nginx_test_ok: false, // nginx -t fallisce
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxReload::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    assert!(
        step.run(&c).is_err(),
        "non ricaricare una config che non passa nginx -t"
    );
    assert!(
        !ops_of(&log)
            .iter()
            .any(|o| matches!(o, Op::ServiceReload(_))),
        "nessun reload di config rotta"
    );
}

/// Trovato dall'e2e in R3: gli undo girano in ordine inverso, quindi
/// `nginx-reload` è il **primo** della fase e le config non sono ancora
/// ripristinate. Ricaricare lì lascerebbe nginx a servire la nostra config
/// dopo che i file sono stati rimossi.
#[test]
fn reload_undo_does_not_reload_before_configs_are_restored() {
    let cfg = MockConfig {
        service_active: true, // nginx era già attivo: è del cliente
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxReload::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    let after_run = ops_of(&log).len();
    step.undo(&c).expect("undo");

    let undo_ops = &ops_of(&log)[after_run..];
    assert!(
        !undo_ops
            .iter()
            .any(|o| matches!(o, Op::ServiceReload(_) | Op::ServiceStop(_))),
        "un nginx preesistente resta attivo e non va ricaricato qui: {undo_ops:?}"
    );
}

/// L'altra metà del fix: `nginx-install` è l'ultimo undo della fase, quindi è
/// lì che il riallineamento deve avvenire — se nginx sopravvive al rollback.
#[test]
fn install_undo_reloads_at_the_end_when_nginx_survives() {
    let cfg = MockConfig {
        installed_packages: ["nginx".to_string()].into_iter().collect(),
        service_enabled: true,
        service_active: true, // nginx del cliente: sopravvive al rollback
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxInstall::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::ServiceReload(s) if s == "nginx")),
        "nginx sopravvissuto va ricaricato per servire la config ripristinata: {ops:?}"
    );
    assert!(
        !ops.iter().any(|o| matches!(o, Op::ServiceStop(_))),
        "un nginx preesistente non va fermato"
    );
}

/// Ma non si ricarica una config che non passa `nginx -t`, nemmeno nell'undo.
#[test]
fn install_undo_does_not_reload_an_invalid_config() {
    let cfg = MockConfig {
        installed_packages: ["nginx".to_string()].into_iter().collect(),
        service_enabled: true,
        service_active: true,
        nginx_test_ok: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxInstall::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|o| matches!(o, Op::ServiceReload(_))),
        "config non valida → nessun reload (come nel run)"
    );
}

#[test]
fn install_undo_does_not_purge_without_aggressive() {
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = NginxInstall::with_ops(Box::new(mock));
    let c = ctx(true, false); // aggressive_rollback = false (default)

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.iter().any(|o| matches!(o, Op::AptInstall(_))));
    assert!(ops.iter().any(|o| matches!(o, Op::ServiceStop(_))));
    assert!(ops.iter().any(|o| matches!(o, Op::ServiceDisable(_))));
    assert!(
        !ops.iter().any(|o| matches!(o, Op::AptPurge(_))),
        "coerenza D3: no purge senza flag"
    );
}

#[test]
fn vhost_rendering_has_no_residue() {
    let c = ctx(true, false);
    let vhost = render_vhost(&c);
    assert!(!vhost.contains("{{"), "nessun placeholder residuo");
    assert!(vhost.contains("127.0.0.1:8069"), "porta Odoo nel proxy");
    validate_vhost(&vhost).expect("vhost valido");
}
