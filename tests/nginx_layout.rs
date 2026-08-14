//! M4: nginx's paths follow the family, and where a concept does not exist the
//! step does not invent it.
//!
//! two divergences are not "a different path": on one family the enabled-sites
//! directory **has no other name, it is absent**, and the default server is not
//! a separate file but a block inside the main configuration. representing
//! those with different constants would have created symlinks where nginx never
//! reads, and looked for a file that does not exist.
//!
//! with `Option` the difference lives in the **data**, and the steps read it
//! instead of deriving it.
//!
//! what does **not** change: the vhost's contents, identical on both families,
//! and the firewall's delta pattern, whose rule token is the same.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::{debian::Debian, fedora::Fedora, Distro, OsFamily};
use invok::state::PreState;
use invok::step::Step;
use invok::steps::{nginx_enable_site::NginxEnableSite, nginx_write_config::NginxWriteConfig};

fn ctx(family: OsFamily) -> Context {
    Context {
        dry_run: false,
        with_nginx: true,
        odoo_version_short: "18".to_string(),
        nginx_server_name: "_".to_string(),
        os_family: family,
        ..Default::default()
    }
}

fn cfg(family: OsFamily) -> MockConfig {
    MockConfig {
        family,
        ..MockConfig::default()
    }
}

// --- the layout: data, not scattered constants ------------------------------

/// the vhost lands where **this** family looks for it, with the right
/// extension.
///
/// the extension is not cosmetic: one family includes **only** files carrying
/// it. a vhost without it would be invisible with nothing to say so — nginx
/// would start, the reload would succeed, Odoo would be unreachable. the same
/// symptomless defect as A-V3-7.
#[test]
fn the_vhost_goes_where_its_family_looks_for_it() {
    assert_eq!(
        Debian::new().nginx_layout().vhost_path("18"),
        std::path::PathBuf::from("/etc/nginx/sites-available/odoo18"),
        "on one family the glob loads any file: no extension needed"
    );
    assert_eq!(
        Fedora::new().nginx_layout().vhost_path("18"),
        std::path::PathBuf::from("/etc/nginx/conf.d/odoo18.conf"),
        "there, without the extension the file is not loaded at all"
    );
}

/// where the concept **does not exist**, the answer is `None` — never an
/// invented path.
#[test]
fn a_missing_concept_is_none_not_a_made_up_path() {
    let debian = Debian::new().nginx_layout();
    assert!(debian.enabled_dir.is_some());
    assert!(debian.default_site.is_some());
    assert_eq!(
        debian.enabled_link("18"),
        Some(std::path::PathBuf::from("/etc/nginx/sites-enabled/odoo18"))
    );

    let fedora = Fedora::new().nginx_layout();
    assert_eq!(
        fedora.enabled_dir, None,
        "on the other the enabled directory has no other name: it does not exist"
    );
    assert_eq!(
        fedora.default_site, None,
        "the default server lives inside the main configuration, not in a file of its own"
    );
    assert_eq!(fedora.enabled_link("18"), None);
}

/// the default site's backup does **not** land in the directory nginx globs.
///
/// that glob loads every file, so a backup left there would still be served and
/// the port would stay occupied. a test already covered this; here the
/// **layout** is checked not to reopen it.
#[test]
fn the_backup_never_lands_where_nginx_globs() {
    for (name, layout) in [
        ("debian", Debian::new().nginx_layout()),
        ("fedora", Fedora::new().nginx_layout()),
    ] {
        if let Some(enabled) = &layout.enabled_dir {
            assert_ne!(
                &layout.default_site_backup_dir, enabled,
                "{name}: the backup would land in the directory nginx globs"
            );
        }
    }
}

// --- the steps read the layout instead of deriving it -----------------------

/// on one family nothing changes: the symlink is created and the default site
/// displaced — the behaviour the CI exercises in the field on both natures, and
/// which M4 was not to touch.
#[test]
fn on_debian_the_site_is_enabled_with_a_symlink() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        default_site_exists: true,
        ..cfg(OsFamily::Debian)
    });
    let mut step = NginxEnableSite::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Debian);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops_log = ops_of(&log);
    assert!(
        ops_log
            .iter()
            .any(|op| matches!(op, Op::CreateSymlink { link, .. }
            if link.to_string_lossy().contains("sites-enabled/odoo18"))),
        "the symlink in the enabled directory is what enables the site: {ops_log:?}"
    );
    assert!(
        ops_log
            .iter()
            .any(|op| matches!(op, Op::RemoveSymlink(p) if p.ends_with("default"))),
        "the default site is moved out of the way to free port 80: {ops_log:?}"
    );
}

/// on the other, **no symlink is created**: writing the vhost is enabling it,
/// and a symlink in a directory that does not exist would be an artifact made
/// for nothing — which the undo would then have to remove.
#[test]
fn on_fedora_writing_the_vhost_is_already_enabling_it() {
    let (ops, log) = MockSystemOps::new(cfg(OsFamily::Fedora));
    let mut step = NginxEnableSite::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops_log = ops_of(&log);
    assert!(
        !ops_log
            .iter()
            .any(|op| matches!(op, Op::CreateSymlink { .. })),
        "no symlink to create on this family: {ops_log:?}"
    );
    assert_eq!(
        serde_json::from_value::<serde_json::Value>(step.snapshot_value())
            .expect("snapshot")
            .get("link")
            .and_then(|v| serde_json::from_value::<PreState>(v.clone()).ok()),
        Some(PreState::Untracked),
        "nothing created means nothing to undo"
    );
}

/// **M4's most delicate point.** there the default site is left alone: it lives
/// inside the main configuration, and removing it would mean rewriting a
/// customer's service configuration.
///
/// the prudent choice of the two, with the consequence declared: a non-matching
/// hostname still gets the welcome page. better that than A-V3-5's risk, where
/// mishandling the default site cost a customer their configuration.
#[test]
fn on_fedora_the_default_server_is_never_touched() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        // even declaring a default site present, on this family the path does
        // not exist and must not be looked for.
        default_site_exists: true,
        ..cfg(OsFamily::Fedora)
    });
    let mut step = NginxEnableSite::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops_log = ops_of(&log);
    assert!(
        !ops_log
            .iter()
            .any(|op| matches!(op, Op::RemoveSymlink(_) | Op::MoveFile { .. })),
        "the default server lives inside the main configuration: we neither remove nor \
         move it. found: {ops_log:?}"
    );
}

/// the vhost is written in the family's directory, and the step knows no
/// constant.
#[test]
fn the_vhost_step_writes_where_the_layout_says() {
    for (family, expected) in [
        (OsFamily::Debian, "/etc/nginx/sites-available/odoo18"),
        (OsFamily::Fedora, "/etc/nginx/conf.d/odoo18.conf"),
    ] {
        let (ops, log) = MockSystemOps::new(cfg(family));
        let mut step = NginxWriteConfig::with_ops(Box::new(ops));
        let c = ctx(family);

        step.snapshot(&c).expect("snapshot");
        step.run(&c).expect("run");

        let ops_log = ops_of(&log);
        assert!(
            ops_log.iter().any(
                |op| matches!(op, Op::MoveFile { dst, .. } if dst.to_string_lossy() == expected)
            ),
            "{family}: the vhost must land in {expected}, found {ops_log:?}"
        );
    }
}

/// the vhost's **contents** do not diverge: one template, and per-family
/// versions would add two things to keep aligned for no gain.
#[test]
fn the_vhost_content_does_not_depend_on_the_family() {
    let reso = |family: OsFamily| invok::steps::nginx_write_config::render_vhost(&ctx(family));
    assert_eq!(
        reso(OsFamily::Debian),
        reso(OsFamily::Fedora),
        "the proxy to the local port is identical everywhere: if it ever diverged it \
         would be for a reason worth writing down, not out of inertia"
    );
}

// --- M4b: SELinux, added because the field asked for it ---------------------

use invok::steps::nginx_selinux::NginxSelinux;

/// **the defect observed in the field.** with a correct vhost, valid
/// validation and a successful reload, the browser gets **502**: SELinux denies
/// nginx the connection to the local service.
///
/// ```text
/// avc: denied { name_connect } for comm="nginx" dest=8069
///      scontext=httpd_t tcontext=unreserved_port_t permissive=0
/// ```
///
/// nothing odd appears in the installer's logs: a defect with no symptom until
/// the first user opens a browser.
#[test]
fn on_fedora_the_proxy_boolean_is_turned_on() {
    let (ops, log) = MockSystemOps::new(cfg(OsFamily::Fedora));
    let mut step = NginxSelinux::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(
        ops_of(&log).iter().any(
            |op| matches!(op, Op::SetSelinuxBoolean { boolean, value: true }
            if boolean == "httpd_can_network_connect")
        ),
        "without this boolean the proxy answers 502: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::CreatedByUs
    );
}

/// on the other family nothing is touched: SELinux is not in use, and mutating
/// the security policy of a system that lacks it would be absurd.
#[test]
fn on_debian_selinux_is_left_alone() {
    let (ops, log) = MockSystemOps::new(cfg(OsFamily::Debian));
    let mut step = NginxSelinux::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Debian);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::SetSelinuxBoolean { .. })),
        "this family has no SELinux: nothing to turn on and nothing to turn off"
    );
}

/// **the protection.** an **already-enabled** boolean is not ours: on a machine
/// hosting other web services it almost always is, and turning it off during a
/// rollback would break somebody else's proxy.
#[test]
fn a_boolean_that_was_already_on_is_never_turned_off() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        selinux_boolean: Some(true),
        ..cfg(OsFamily::Fedora)
    });
    let mut step = NginxSelinux::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::Preexisting,
        "it was on before us"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::SetSelinuxBoolean { .. })),
        "neither on nor off: not ours to touch"
    );
}

/// the undo turns off **only** what we turned on.
#[test]
fn what_we_turned_on_we_turn_off() {
    let (ops, log) = MockSystemOps::new(cfg(OsFamily::Fedora));
    let mut step = NginxSelinux::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::SetSelinuxBoolean { value: false, .. })),
        "the boolean we turned on goes back as it was: it is a persistent security \
         policy, not a session setting"
    );
}

/// **unqueryable is not off.** with no answer the policy is left alone.
///
/// the same distinction between blindness and absence as A5.1-bis: acting on an
/// "I do not know" means writing to somebody else's system on information we do
/// not have.
#[test]
fn an_unreadable_policy_is_not_a_policy_to_write() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        selinux_boolean: None,
        ..cfg(OsFamily::Fedora)
    });
    let mut step = NginxSelinux::with_ops(Box::new(ops));
    let c = ctx(OsFamily::Fedora);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::SetSelinuxBoolean { .. })),
        "SELinux cannot be queried: in doubt the policy is not mutated"
    );
}

/// **the hole mutation testing found**, for the second time the same way.
///
/// the tests above go through the mock, which has its *own* boolean name: the
/// real constant was exercised by nothing, and a mutation naming a different
/// boolean survived the whole suite. in the field that would have enabled the
/// **wrong** one — the other governs proxying to *remote* hosts, not the local
/// connection the audit log shows — so the same 502, plus a security policy
/// changed for nothing.
///
/// the same lesson as M3: when the mock replicates a production decision, that
/// decision must be exercised **where it is written** too.
#[test]
fn the_boolean_is_the_one_the_kernel_actually_denies() {
    use invok::distro::{debian::Debian, fedora::Fedora, Distro};

    let selinux = Fedora::new()
        .selinux()
        .expect("on this family SELinux is in use")
        .nginx_proxy_boolean()
        .to_string();
    assert_eq!(
        selinux, "httpd_can_network_connect",
        "this is the boolean governing `name_connect` to a local port, exactly what the \
         audit log shows denied. the other one covers proxying to remote hosts and \
         would unblock nothing"
    );

    assert!(
        Debian::new().selinux().is_none(),
        "on this family SELinux is not in use: the trait is not implemented, so no branch \
         is left that cannot run"
    );
}

/// the firewall message names **the family's** tool.
///
/// naming the wrong one — observed in the field — sends the reader looking for
/// something their machine does not have.
#[test]
fn the_firewall_is_called_by_its_real_name() {
    use invok::distro::{debian::Debian, fedora::Fedora, Distro};

    assert_eq!(Debian::new().firewall().name(), "ufw");
    assert_eq!(Fedora::new().firewall().name(), "firewalld");
}
