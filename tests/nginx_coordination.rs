//! nginx coordination: a complete rollback through the engine restores the
//! default site and removes only the firewall delta.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{ops_of, MockConfig, MockSystemOps, Op, OpLog};
use invok::context::Context;
use invok::engine::Installer;
use invok::step::Step;
use invok::steps::nginx_enable_site::NginxEnableSite;
use invok::steps::nginx_firewall::NginxFirewall;
use invok::steps::nginx_install::NginxInstall;
use invok::steps::nginx_reload::NginxReload;
use invok::steps::nginx_write_config::NginxWriteConfig;
use invok::steps::noop::NoopStep;

#[test]
fn full_rollback_restores_default_site_and_removes_only_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log: OpLog = Arc::new(Mutex::new(Vec::new()));

    let mk = |cfg: MockConfig| MockSystemOps::with_log(cfg, Arc::clone(&log));
    let ufw_rules: HashSet<String> = ["80/tcp"].iter().map(|s| s.to_string()).collect();

    // the default site is present, and the firewall is active with port 80
    // already open.
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
        nginx_open_https_port: true, // desidera 80 + 443 → delta = 443
        nginx_server_name: "_".to_string(),
        odoo_version_short: "18".to_string(),
        port: 8069,
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"));

    let mut installer = Installer::new();
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "the final step triggers the rollback"
    );

    let ops = ops_of(&log);
    let default_site = PathBuf::from("/etc/nginx/sites-enabled/default");

    // the default site is back.
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::CreateSymlink { link, .. } if *link == default_site)),
        "the rollback must restore the customer's default site"
    );
    // only the delta is removed, never the pre-existing rule.
    assert!(ops.contains(&Op::UfwDelete("443/tcp".to_string())));
    assert!(
        !ops.contains(&Op::UfwDelete("80/tcp".to_string())),
        "the customer's port 80 rule is not removed"
    );
}
