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

// --- M4b: SELinux, aggiunto perché il campo l'ha chiesto --------------------

use odoo_installer::steps::nginx_selinux::NginxSelinux;

/// **Il difetto osservato in campo.** Su Fedora, con il vhost corretto,
/// `nginx -t` valido e il reload riuscito, il browser riceve **502**: SELinux
/// nega a nginx la connessione verso `127.0.0.1:8069`.
///
/// ```text
/// avc: denied { name_connect } for comm="nginx" dest=8069
///      scontext=httpd_t tcontext=unreserved_port_t permissive=0
/// ```
///
/// Nei log dell'installer non compare nulla di anomalo: è un difetto senza
/// sintomo fino al primo utente che apre il browser.
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
        "senza questo boolean il proxy risponde 502: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::CreatedByUs
    );
}

/// Su Debian non si tocca nulla: SELinux non è in uso, e mutare la politica di
/// sicurezza di un sistema che non ce l'ha sarebbe assurdo.
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
        "questa famiglia non ha SELinux: niente da accendere e niente da spegnere"
    );
}

/// **La protezione.** Un boolean **già acceso** non è nostro: su una macchina
/// che ospita altri servizi web lo è quasi sempre, e spegnerlo al rollback
/// romperebbe il proxy di qualcun altro.
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
        "era acceso prima di noi"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::SetSelinuxBoolean { .. })),
        "né acceso né spento: non è nostro da toccare"
    );
}

/// L'undo spegne **solo** ciò che abbiamo acceso noi.
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
        "il boolean acceso da noi va rimesso com'era: è una politica di sicurezza \
         persistente, non un'impostazione di sessione"
    );
}

/// **Non interrogabile ≠ spento.** Se `getsebool` non risponde — SELinux
/// disabilitato, `policycoreutils` assente — non si tocca la politica.
///
/// È la stessa distinzione fra cecità e assenza di A5.1-bis: agire su un
/// «non lo so» significa scrivere sul sistema di qualcun altro sulla base di
/// un'informazione che non abbiamo.
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
        "SELinux non interrogabile: nel dubbio non si muta la politica"
    );
}

/// **Il buco che la validazione per mutazione ha trovato**, per la seconda volta
/// nello stesso modo.
///
/// I test sopra passano dal mock, che ha una *sua* `nginx_proxy_boolean`: la
/// costante vera di `FedoraSelinux` non era esercitata da niente, e la mutazione
/// «usa `httpd_can_network_relay`» sopravviveva a tutta la suite. In campo
/// l'effetto sarebbe stato accendere il boolean **sbagliato** — `relay` governa
/// il proxy verso host *remoti*, non il `name_connect` locale che il log di
/// `ausearch` mostra — quindi 502 identico, con in più una politica di sicurezza
/// modificata per niente.
///
/// È la stessa lezione di M3 (`postgres_data_dir`): quando il mock replica una
/// decisione della produzione, la decisione va provata **anche dov'è scritta**.
#[test]
fn the_boolean_is_the_one_the_kernel_actually_denies() {
    use odoo_installer::distro::{debian::Debian, fedora::Fedora, Distro};

    let selinux = Fedora::new()
        .selinux()
        .expect("su Fedora SELinux è in uso")
        .nginx_proxy_boolean()
        .to_string();
    assert_eq!(
        selinux, "httpd_can_network_connect",
        "è il boolean che governa `name_connect` verso una porta locale, che è \
         esattamente ciò che ausearch mostra negato. `httpd_can_network_relay` \
         riguarda il proxy verso host remoti e non sbloccherebbe nulla"
    );

    assert!(
        Debian::new().selinux().is_none(),
        "su questa famiglia SELinux non è in uso: il trait non è implementato, \
         così non resta nessun ramo che non possa eseguire"
    );
}

/// Il messaggio del firewall nomina lo strumento **della famiglia**.
///
/// «ufw non trovato» detto su Fedora — osservato in campo — manda a cercare uno
/// strumento che lì non esiste. Stessa classe di «esegui `apt-get update`» detto
/// a chi ha dnf.
#[test]
fn the_firewall_is_called_by_its_real_name() {
    use odoo_installer::distro::{debian::Debian, fedora::Fedora, Distro};

    assert_eq!(Debian::new().firewall().name(), "ufw");
    assert_eq!(Fedora::new().firewall().name(), "firewalld");
}
