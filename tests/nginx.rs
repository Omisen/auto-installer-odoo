//! Test della fase Nginx (Fase 9): gating, protezione default site, delta ufw.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

// --- A-V3-5: la natura del default site, non solo la sua esistenza ----------

use odoo_installer::steps::nginx_enable_site::{DefaultSite, NginxEnableSiteSnapshot};
use odoo_installer::system_ops::PathKind;

fn enable_site_snapshot(step: &NginxEnableSite) -> NginxEnableSiteSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

/// **Il ripristino dev'essere fedele.** Se il default site del cliente puntava
/// a un vhost con un altro nome, l'undo deve riportarlo **là**, non al target
/// standard della distribuzione.
///
/// Prima lo snapshot era un `bool` e l'undo ricreava sempre un symlink verso
/// `/etc/nginx/sites-available/default`: la config non tornava com'era, tornava
/// com'è *di solito*. Per un progetto che ripristina il `.bashrc` byte-per-byte
/// era un doppio standard.
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

/// **Un file regolare non si cancella.** `symlink_exists` rispondeva `true`
/// anche per un file vero e `remove_symlink` è `fs::remove_file`: un
/// amministratore che avesse scritto `sites-enabled/default` come file si vedeva
/// il contenuto distrutto, e l'undo gli restituiva un symlink al default della
/// distro. Non un residuo: una perdita di configurazione.
#[test]
fn a_regular_default_site_is_moved_to_a_backup_never_deleted() {
    let cfg = MockConfig {
        default_site_exists: true,
        default_site_kind: Some(PathKind::RegularFile),
        // Il backup, una volta creato, esiste: senza questo l'undo lo
        // troverebbe assente e si asterrebbe (comportamento corretto, ma qui
        // stiamo verificando il ripristino).
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

    // Il backup non può restare in sites-enabled: nginx include `sites-enabled/*`
    // — ogni file, non solo i .conf — quindi verrebbe ricaricato e la porta 80
    // resterebbe occupata. Sarebbe lo stesso difetto con un altro nome.
    assert!(
        !spostato.starts_with("/etc/nginx/sites-enabled"),
        "il backup non deve restare dove nginx lo ricaricherebbe: {}",
        spostato.display()
    );

    // E l'undo lo rimette esattamente dov'era.
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

/// Directory (o symlink illeggibile) al posto del default site: non sappiamo
/// trattarlo, quindi non lo tocchiamo. Fail-closed: meglio una porta 80 occupata
/// e un avviso che una rimozione alla cieca.
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

/// Retrocompatibilità: uno stato persistito **prima** della R11 non ha la natura
/// del default site, solo il `bool`. Deve restare consumabile — e ricadere sul
/// comportamento storico, che è il meglio ricavabile da quell'informazione.
///
/// Renderlo illeggibile significherebbe lasciare la porta 80 senza il default
/// site che avevamo tolto: la stessa cura di retrocompatibilità dell'
/// `InstallConfig` in R4.
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

// --- A-V3-6: il flag dice cosa fa, e il vhost non finge -----------------------

/// **La proprietà che dà il nome al flag.** `--open-https-port` tocca il
/// firewall e **solo** il firewall: il vhost generato è identico con e senza.
///
/// Prima si chiamava `--enable-ssl` e prometteva TLS. Il vhost però non ha né
/// mai ha avuto un listener su 443 — il blocco era interamente commentato — e i
/// placeholder dei certificati venivano sostituiti *dentro quei commenti*. Chi
/// lo passava otteneva una porta aperta verso nulla e la convinzione di avere
/// TLS: peggio di un flag assente.
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

/// Il vhost non contiene un blocco 443 — né attivo né commentato — e nessun
/// riferimento a certificati.
///
/// Il blocco commentato non era innocuo: descriveva una configurazione che
/// nessuno generava, prometteva che *«sarà ignorato da Nginx se il certificato
/// non esiste»* (falso: nginx rifiuta di partire) e citava `lib/nginx.sh`, un
/// file dell'era Bash che non esiste più. Un template che dice di sé cose non
/// vere è documentazione al contrario.
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
    // E resta un vhost valido, senza placeholder residui.
    validate_vhost(&reso).expect("nessun placeholder residuo");
    assert!(reso.contains("listen 80"), "il vhost serve la porta 80");
}

/// Il firewall è l'unico effetto reale, e c'è: la 443 si apre.
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
