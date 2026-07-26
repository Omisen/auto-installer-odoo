//! Coordinamento Nginx (Fase 9): rollback completo via engine — il default site
//! è ripristinato e solo il delta firewall è rimosso.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{ops_of, MockConfig, MockSystemOps, Op, OpLog};
use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::step::Step;
use odoo_installer::steps::nginx_enable_site::NginxEnableSite;
use odoo_installer::steps::nginx_firewall::NginxFirewall;
use odoo_installer::steps::nginx_install::NginxInstall;
use odoo_installer::steps::nginx_reload::NginxReload;
use odoo_installer::steps::nginx_write_config::NginxWriteConfig;
use odoo_installer::steps::noop::NoopStep;

#[test]
fn full_rollback_restores_default_site_and_removes_only_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log: OpLog = Arc::new(Mutex::new(Vec::new()));

    let mk = |cfg: MockConfig| MockSystemOps::with_log(cfg, Arc::clone(&log));
    let ufw_rules: HashSet<String> = ["80/tcp"].iter().map(|s| s.to_string()).collect();

    // Install/write/reload usano config default; enable_site vede il default
    // site presente; firewall ha ufw attivo con 80 già aperta.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NginxInstall::with_ops(Box::new(mk(MockConfig::default())))),
        Box::new(NginxWriteConfig::with_ops(Box::new(mk(MockConfig {
            real_fs: false,
            ..Default::default()
        })))),
        Box::new(NginxEnableSite::with_ops(Box::new(mk(MockConfig {
            default_site_exists: true,
            ..Default::default()
        })))),
        Box::new(NginxFirewall::with_ops(Box::new(mk(MockConfig {
            ufw_available: true,
            ufw_active: true,
            existing_ufw_rules: ufw_rules,
            ..Default::default()
        })))),
        Box::new(NginxReload::with_ops(Box::new(mk(MockConfig::default())))),
        Box::new(NoopStep::new("boom").fail_on_run()),
    ];

    let ctx = Context {
        with_nginx: true,
        nginx_enable_ssl: true, // desidera 80 + 443 → delta = 443
        nginx_server_name: "_".to_string(),
        odoo_version_short: "18".to_string(),
        port: 8069,
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"));

    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err(), "lo step finale innesca il rollback");

    let ops = ops_of(&log);
    let default_site = PathBuf::from("/etc/nginx/sites-enabled/default");

    // Default site ripristinato.
    assert!(
        ops.iter().any(|o| matches!(o, Op::CreateSymlink { link, .. } if *link == default_site)),
        "il rollback deve ripristinare il default site del cliente"
    );
    // Firewall: rimosso solo il delta (443), mai la 80 preesistente.
    assert!(ops.contains(&Op::UfwDelete("443/tcp".to_string())));
    assert!(!ops.contains(&Op::UfwDelete("80/tcp".to_string())), "la 80 del cliente non va rimossa");
}
