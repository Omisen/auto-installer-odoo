//! the nginx phase: gating, the default-site protection, the firewall delta.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::nginx_enable_site::NginxEnableSite;
use invok::steps::nginx_firewall::NginxFirewall;
use invok::steps::nginx_install::NginxInstall;
use invok::steps::nginx_reload::NginxReload;
use invok::steps::nginx_write_config::{render_vhost, validate_vhost, NginxWriteConfig};

fn ctx(with_nginx: bool, ssl: bool) -> Context {
    Context {
        with_nginx,
        nginx_open_https_port: ssl,
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

// --- gating: without the flag, everything is inert --------------------------

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

// --- the default site: the phase's protection -------------------------------

#[test]
fn default_site_removed_then_restored() {
    // THE protective test: the default site existed, the run removes it, and
    // the undo restores the customer's config as it was.
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
    // the run removes it.
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveSymlink(p) if *p == default_site())));
    // the undo RESTORES it.
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::CreateSymlink { link, .. } if *link == default_site())),
        "undo deve ripristinare il default site: {ops:?}"
    );
}

#[test]
fn absent_default_site_is_not_invented() {
    // it did not exist: the run does not touch it and the undo does not invent
    // it.
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

// --- the firewall: the delta pattern ----------------------------------------

#[test]
fn firewall_undo_removes_only_the_delta() {
    // one rule was already there and one is ours: only ours is removed.
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

// --- reload and install -----------------------------------------------------

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

/// found by the end-to-end tests: undos run in reverse, so the reload step is
/// the **first** of the phase and the configurations are not restored yet.
/// reloading there would leave nginx serving our config after its files are
/// gone.
#[test]
fn reload_undo_does_not_reload_before_configs_are_restored() {
    let cfg = MockConfig {
        service_active: true, // nginx was already active: it is the customer's
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

/// the other half of the fix: the install step is the phase's last undo, so
/// that is where the realignment belongs — if nginx survives the rollback.
#[test]
fn install_undo_reloads_at_the_end_when_nginx_survives() {
    let cfg = MockConfig {
        installed_packages: ["nginx".to_string()].into_iter().collect(),
        service_enabled: true,
        service_active: true, // the customer's nginx: survives the rollback
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

/// but a config that fails validation is never reloaded, not even in an undo.
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
    assert!(ops.iter().any(|o| matches!(o, Op::PkgInstall(_))));
    assert!(ops.iter().any(|o| matches!(o, Op::ServiceStop(_))));
    assert!(ops.iter().any(|o| matches!(o, Op::ServiceDisable(_))));
    assert!(
        !ops.iter().any(|o| matches!(o, Op::PkgRemove(_))),
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

// --- A-V3-5: the default site's nature, not just its existence --------------

use invok::steps::nginx_enable_site::{DefaultSite, NginxEnableSiteSnapshot};
use invok::system_ops::PathKind;

fn enable_site_snapshot(step: &NginxEnableSite) -> NginxEnableSiteSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

/// **restoration must be faithful.** if the customer's default site pointed at
/// a differently named vhost, the undo must point it back **there**, not at the
/// distribution's standard target.
///
/// the snapshot used to be a boolean and the undo always recreated the standard
/// link: the config did not come back as it was, it came back as it *usually*
/// is.
#[test]
fn a_non_standard_default_site_is_restored_to_its_own_target() {
    let cliente = PathBuf::from("/etc/nginx/sites-available/vhost-del-cliente");
    let cfg = MockConfig {
        default_site_exists: true,
        default_site_kind: Some(PathKind::Symlink {
            target: cliente.clone(),
        }),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        enable_site_snapshot(&step).default_site,
        Some(DefaultSite::Symlink {
            target: cliente.clone()
        }),
        "lo snapshot deve registrare il target, non solo l'esistenza"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(
            o,
            Op::CreateSymlink { src, link } if *src == cliente && *link == default_site()
        )),
        "l'undo deve ricreare il symlink verso il target ORIGINALE: {ops:?}"
    );
    assert!(
        !ops.iter().any(|o| matches!(
            o,
            Op::CreateSymlink { src, link }
                if *link == default_site()
                    && src.as_path() == Path::new("/etc/nginx/sites-available/default")
        )),
        "nessun ripristino verso il target standard: la config del cliente non è quella: {ops:?}"
    );
}

/// **a regular file is not deleted.** the existence check answered `true` for a
/// real file too, and the removal is a plain file removal: an administrator who
/// had written that path as a file saw its contents destroyed, and got a
/// symlink to the distro default back. not a leftover: a loss of configuration.
#[test]
fn a_regular_default_site_is_moved_to_a_backup_never_deleted() {
    let cfg = MockConfig {
        default_site_exists: true,
        default_site_kind: Some(PathKind::RegularFile),
        // once created the backup exists: without this the undo would find it
        // missing and abstain — correct, but not what we are checking.
        path_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    assert!(
        !ops.iter()
            .any(|o| matches!(o, Op::RemoveSymlink(p) if *p == default_site())),
        "un file regolare non va MAI rimosso: {ops:?}"
    );
    let spostato = ops
        .iter()
        .find_map(|o| match o {
            Op::MoveFile { src, dst } if *src == default_site() => Some(dst.clone()),
            _ => None,
        })
        .expect("il file va spostato in un backup");

    // the backup cannot stay in the enabled directory, which nginx globs: it
    // would be loaded and the port would stay occupied — the same defect under
    // another name.
    assert!(
        !spostato.starts_with("/etc/nginx/sites-enabled"),
        "il backup non deve restare dove nginx lo ricaricherebbe: {}",
        spostato.display()
    );

    // and the undo puts it back exactly where it was.
    step.undo(&c).expect("undo");
    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(
            o,
            Op::MoveFile { src, dst } if *src == spostato && *dst == default_site()
        )),
        "l'undo deve rimettere il file al suo posto: {ops:?}"
    );
}

/// a directory, or an unreadable symlink, in the default site's place: we do
/// not know how to treat it, so we do not. better an occupied port and a
/// warning than a blind removal.
#[test]
fn an_unknown_default_site_is_left_alone() {
    let cfg = MockConfig {
        default_site_exists: true,
        default_site_kind: Some(PathKind::Other),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        !ops.iter().any(|o| matches!(
            o,
            Op::RemoveSymlink(p) | Op::MoveFile { src: p, .. } if *p == default_site()
        )),
        "su qualcosa che non sappiamo trattare non si agisce: {ops:?}"
    );
}

/// compatibility: a state persisted **before** R11 has only the boolean. it
/// must stay consumable and fall back to the historical behaviour, the best
/// that information allows.
///
/// making it unreadable would leave the port without the default site we took
/// away.
#[test]
fn a_pre_r11_snapshot_still_restores_the_default_site() {
    let legacy = serde_json::json!({
        "link": "CreatedByUs",
        "default_site_existed": true
    });

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = NginxEnableSite::with_ops(Box::new(mock));
    step.rehydrate(&legacy)
        .expect("uno stato pre-R11 resta leggibile");

    let snap = enable_site_snapshot(&step);
    assert!(snap.default_site_existed);
    assert_eq!(
        snap.default_site, None,
        "il campo nuovo resta assente: è ciò che segnala uno stato vecchio"
    );

    step.undo(&ctx(true, false)).expect("undo");
    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(
            o,
            Op::CreateSymlink { src, link }
                if *link == default_site()
                    && src.as_path() == Path::new("/etc/nginx/sites-available/default")
        )),
        "senza la natura registrata si ricade sul target standard: {ops:?}"
    );
}

// --- A-V3-6: the flag says what it does, and the vhost does not pretend -----

/// **the property the flag is named for**: it touches the firewall and **only**
/// the firewall, so the generated vhost is identical with and without it.
///
/// it used to be called `--enable-ssl` and promised TLS. the vhost never had a
/// 443 listener — the block was entirely commented out — and the certificate
/// placeholders were substituted *inside those comments*. passing it gave an
/// open port towards nothing and the belief of having TLS: worse than no flag
/// at all.
#[test]
fn opening_the_https_port_does_not_change_the_vhost() {
    let senza = render_vhost(&ctx(true, false));
    let con = render_vhost(&ctx(true, true));

    assert_eq!(
        senza, con,
        "il flag non deve toccare il vhost: TLS è compito di `certbot --nginx`, \
         che il vhost lo riscrive da sé"
    );
}

/// the vhost contains no 443 block — active or commented — and no certificate
/// reference.
///
/// the commented block was not harmless: it described a configuration nobody
/// generated, claimed nginx would ignore it without a certificate (false: nginx
/// refuses to start) and cited a file from the Bash era that no longer exists.
/// a template that says untrue things about itself is documentation in reverse.
#[test]
fn the_vhost_makes_no_tls_promises() {
    let reso = render_vhost(&ctx(true, true));

    for bugia in [
        "listen 443",
        "ssl_certificate",
        "NGINX_CERT_PATH",
        "lib/nginx.sh",
    ] {
        assert!(
            !reso.contains(bugia),
            "il vhost non deve contenere '{bugia}': non configura TLS (A-V3-6)"
        );
    }
    // and it stays a valid vhost, with no placeholder left.
    validate_vhost(&reso).expect("nessun placeholder residuo");
    assert!(reso.contains("listen 80"), "il vhost serve la porta 80");
}

/// the firewall is the only real effect, and it happens: the port opens.
#[test]
fn opening_the_https_port_opens_443_on_the_firewall() {
    let cfg = MockConfig {
        ufw_available: true,
        ufw_active: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxFirewall::with_ops(Box::new(mock));
    let c = ctx(true, true);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(
        ops_of(&log).contains(&Op::UfwAllow("443/tcp".to_string())),
        "l'unico effetto del flag deve esserci davvero"
    );
}

/// A-V3-7's consequence: on a machine that already has a similar-looking rule,
/// the port **must still be opened**.
///
/// the defect did not look like an error: nginx was installed, configured and
/// reloaded without a hitch, and stayed unreachable from outside. no odd line
/// in the report — the symptom was absent, not obscure.
///
/// the behavioural half of the guard: the pure one proves the predicate, this
/// proves what follows from it. writing it cost making the mock faithful, where
/// it used to answer with a set membership the real tool does not have.
#[test]
fn port_80_is_opened_even_when_8080_is_already_allowed() {
    let cfg = MockConfig {
        ufw_available: true,
        ufw_active: true,
        existing_ufw_rules: rules(&["8080/tcp"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = NginxFirewall::with_ops(Box::new(mock));
    let c = ctx(true, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::UfwAllow("80/tcp".to_string())),
        "80/tcp non è presente solo perché lo è 8080/tcp: senza questa apertura \
         nginx resta irraggiungibile e nulla lo segnala (A-V3-7): {ops:?}"
    );

    // and the converse holds: a genuinely present rule is not re-added.
    step.undo(&c).expect("undo");
    let ops = ops_of(&log);
    assert!(
        !ops.contains(&Op::UfwDelete("8080/tcp".to_string())),
        "mai toccare una regola preesistente del cliente: {ops:?}"
    );
}

/// A-V3-12: the vhost's logs carry the **version** in their names.
///
/// they were hardcoded, so two instances on one machine — the migration case —
/// wrote to the same file and neither had a readable log. the files survive the
/// rollback, being logs, but at least one can tell whose they are.
#[test]
fn the_vhost_logs_carry_the_version_in_their_name() {
    let mut c = ctx(true, false);
    c.odoo_version_short = "17".to_string();
    let reso = render_vhost(&c);

    assert!(
        reso.contains("/var/log/nginx/odoo17.access.log"),
        "il nome del log deve seguire la versione installata"
    );
    assert!(reso.contains("/var/log/nginx/odoo17.error.log"));
    assert!(
        !reso.contains("odoo18"),
        "nessun riferimento cablato a una versione diversa da quella installata"
    );
    validate_vhost(&reso).expect("nessun placeholder residuo");
}
