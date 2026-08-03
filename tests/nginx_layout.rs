//! M4 — i percorsi di nginx seguono la famiglia, e dove un concetto non esiste
//! lo step non lo inventa.
//!
//! # Le due divergenze che non sono «un percorso diverso»
//!
//! Su Fedora `sites-enabled` **non ha un altro nome: non c'è**, e il server di
//! default non è un file separato — è un blocco dentro `/etc/nginx/nginx.conf`.
//! Rappresentarle con costanti diverse avrebbe fatto creare symlink in una
//! directory che nginx non legge, e cercare un file che non esiste.
//!
//! Con `Option` la differenza sta nei **dati**, e gli step la leggono invece di
//! dedurla.
//!
//! # Cosa NON cambia
//!
//! Il contenuto del vhost: `templates/nginx.conf.tpl` è identico sulle due
//! famiglie, e non c'era ragione di renderlo divergente. Il pattern delta del
//! firewall neanche — il token della regola è lo stesso, ed è il motivo per cui
//! `nginx-firewall` non è stato toccato né in M2 né qui.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::distro::{debian::Debian, fedora::Fedora, Distro, OsFamily};
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::{
    nginx_enable_site::NginxEnableSite, nginx_write_config::NginxWriteConfig,
};

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

// --- Il layout: dati, non costanti sparse ------------------------------------

/// Il vhost finisce dove **questa** famiglia lo cerca, con l'estensione giusta.
///
/// L'estensione non è cosmesi: `nginx.conf` su Fedora include `conf.d/*.conf` —
/// **solo** quelli. Un vhost senza estensione lì sarebbe invisibile e nulla lo
/// direbbe: nginx partirebbe, il reload riuscirebbe, e Odoo non sarebbe
/// raggiungibile. È lo stesso difetto senza sintomo di A-V3-7.
#[test]
fn the_vhost_goes_where_its_family_looks_for_it() {
    assert_eq!(
        Debian::new().nginx_layout().vhost_path("18"),
        std::path::PathBuf::from("/etc/nginx/sites-available/odoo18"),
        "su Debian `sites-enabled/*` include qualunque file: nessuna estensione"
    );
    assert_eq!(
        Fedora::new().nginx_layout().vhost_path("18"),
        std::path::PathBuf::from("/etc/nginx/conf.d/odoo18.conf"),
        "su Fedora senza `.conf` il file non viene caricato affatto"
    );
}

/// Dove il concetto **non esiste**, la risposta è `None` — non un percorso
/// inventato.
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
        "su Fedora `sites-enabled` non ha un altro nome: non c'è"
    );
    assert_eq!(
        fedora.default_site, None,
        "il server di default vive dentro nginx.conf, non in un file a sé"
    );
    assert_eq!(fedora.enabled_link("18"), None);
}

/// Il backup del default site **non** finisce nella directory che nginx include.
///
/// `sites-enabled/*` carica ogni file, non solo i `.conf`: un backup lasciato lì
/// verrebbe servito lo stesso e la porta 80 resterebbe occupata — cioè il
/// difetto che stiamo correggendo, con un altro nome. C'era già un test su
/// questo prima di M4; qui si verifica che il **layout** non lo riapra.
#[test]
fn the_backup_never_lands_where_nginx_globs() {
    for (nome, layout) in [
        ("debian", Debian::new().nginx_layout()),
        ("fedora", Fedora::new().nginx_layout()),
    ] {
        if let Some(enabled) = &layout.enabled_dir {
            assert_ne!(
                &layout.default_site_backup_dir, enabled,
                "{nome}: il backup finirebbe nella directory che nginx include con un glob"
            );
        }
    }
}

// --- Gli step leggono il layout invece di dedurlo ----------------------------

/// Su Debian nulla cambia: symlink creato, default site spiazzato. È il
/// comportamento che la CI verifica in campo su entrambe le nature del default
/// site, e M4 non doveva toccarlo.
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
        "il symlink in sites-enabled è ciò che abilita il sito: {ops_log:?}"
    );
    assert!(
        ops_log
            .iter()
            .any(|op| matches!(op, Op::RemoveSymlink(p) if p.ends_with("default"))),
        "il default site va tolto di mezzo per liberare la porta 80: {ops_log:?}"
    );
}

/// Su Fedora **non si crea alcun symlink**: scrivere il vhost in `conf.d` è già
/// abilitarlo, e un symlink in una directory inesistente sarebbe un artefatto
/// creato per niente — che l'undo dovrebbe poi rimuovere.
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
        "nessun symlink da creare su questa famiglia: {ops_log:?}"
    );
    assert_eq!(
        serde_json::from_value::<serde_json::Value>(step.snapshot_value())
            .expect("snapshot")
            .get("link")
            .and_then(|v| serde_json::from_value::<PreState>(v.clone()).ok()),
        Some(PreState::Untracked),
        "niente creato = niente da annullare"
    );
}

/// **Il punto più delicato di M4.** Su Fedora il default site non si tocca:
/// vive dentro `nginx.conf`, e rimuoverlo significherebbe riscrivere la
/// configurazione principale di un servizio del cliente.
///
/// È la scelta più prudente delle due, e la conseguenza va dichiarata (README):
/// su una Fedora con nginx appena installato, un hostname che non combacia con
/// `NGINX_SERVER_NAME` continua a ricevere la pagina di benvenuto. Meglio quello
/// che il rischio di A-V3-5, dove trattare male il default site è costato la
/// distruzione di configurazione del cliente.
#[test]
fn on_fedora_the_default_server_is_never_touched() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        // Anche dichiarando un default site presente, su questa famiglia il
        // percorso non esiste e non deve essere cercato.
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
        "il server di default sta dentro nginx.conf: non lo rimuoviamo e non lo \
         spostiamo. Trovato: {ops_log:?}"
    );
}

/// Il vhost si scrive nella directory della famiglia, e lo step non conosce
/// alcuna costante.
#[test]
fn the_vhost_step_writes_where_the_layout_says() {
    for (family, atteso) in [
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
                |op| matches!(op, Op::MoveFile { dst, .. } if dst.to_string_lossy() == atteso)
            ),
            "{family}: il vhost deve finire in {atteso}, trovato {ops_log:?}"
        );
    }
}

/// Il **contenuto** del vhost non diverge: è lo stesso template, e renderlo
/// diverso per famiglia aggiungerebbe due cose da mantenere allineate senza
/// alcun guadagno.
#[test]
fn the_vhost_content_does_not_depend_on_the_family() {
    let reso =
        |family: OsFamily| odoo_installer::steps::nginx_write_config::render_vhost(&ctx(family));
    assert_eq!(
        reso(OsFamily::Debian),
        reso(OsFamily::Fedora),
        "il proxy verso 127.0.0.1:8069 è identico ovunque: se un giorno divergesse, \
         sarebbe per una ragione da scrivere, non per inerzia"
    );
}
