//! Test di rollback **end-to-end** (Fase 12, G7): la prova della promessa
//! "chirurgico" a livello di sistema.
//!
//! Si parte da uno stato iniziale del [`SystemModel`], si esegue la sequenza
//! reale di step, si inietta un fallimento, e si verifica che dopo il rollback
//! lo stato sia **identico** all'iniziale — e che le risorse preesistenti del
//! cliente sopravvivano intatte.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use common::MockDownloader;
use odoo_installer::checks::OsInfo;
use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::error::StepError;
use odoo_installer::secret::Secret;
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::AptPackagesStep;
use odoo_installer::steps::clone_odoo_repo::CloneOdooRepo;
use odoo_installer::steps::create_database::CreateDatabase;
use odoo_installer::steps::create_db_role::CreateDbRole;
use odoo_installer::steps::create_odoo_user::CreateOdooUser;
use odoo_installer::steps::create_virtualenv::CreateVirtualenv;
use odoo_installer::steps::generate_config::GenerateConfig;
use odoo_installer::steps::initialize_odoo_database::InitializeOdooDatabase;
use odoo_installer::steps::install_wkhtmltopdf::InstallWkhtmltopdf;
use odoo_installer::steps::nginx_enable_site::NginxEnableSite;
use odoo_installer::steps::nginx_firewall::NginxFirewall;
use odoo_installer::steps::nginx_install::NginxInstall;
use odoo_installer::steps::nginx_reload::NginxReload;
use odoo_installer::steps::nginx_write_config::NginxWriteConfig;
use odoo_installer::steps::noop::NoopStep;
use odoo_installer::steps::patch_bashrc::PatchBashrc;
use odoo_installer::steps::setup_systemd::SetupSystemd;
use odoo_installer::steps::write_control_script::WriteControlScript;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn paths(items: &[&str]) -> HashSet<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

/// Stato iniziale "macchina fresca": /opt/odoo e la home utente esistono, il
/// .bashrc dell'utente ha il suo contenuto, nient'altro di nostro.
fn fresh_state() -> ModelState {
    let mut contents = HashMap::new();
    contents.insert(PathBuf::from(BASHRC), BASHRC_ORIG.to_string());
    ModelState {
        paths: paths(&[HOME, SUDO_HOME, BASHRC]),
        file_contents: contents,
        packages: set(&["coreutils"]),
        sudo_home: Some(SUDO_HOME.to_string()),
        ..Default::default()
    }
}

fn ctx(state_path: PathBuf, aggressive: bool) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        db_password: Secret::default(),
        admin_passwd: Secret::new("s3cret"),
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(INSTALL),
        port: 8069,
        with_nginx: false,
        dry_run: false,
        aggressive_rollback: aggressive,
        sudo_user: Some("alice".to_string()),
        state_path,
        ..Default::default()
    }
}

/// La sequenza reale (mockabile) di step, condividendo lo stesso modello.
/// PrepareOptRoot è escluso (usa `std::fs` diretto; il suo ciclo su /opt/odoo è
/// coperto dai test di Fase 2); qui /opt/odoo è parte dello stato iniziale.
fn chain(model: &SystemModel) -> Vec<Box<dyn Step>> {
    vec![
        Box::new(CreateOdooUser::with_ops(model.boxed())),
        Box::new(AptPackagesStep::odoo_dependencies_with_ops(model.boxed())),
        Box::new(SetupPostgres::with_ops(model.boxed())),
        Box::new(CreateDbRole::with_ops(model.boxed())),
        Box::new(CreateDatabase::with_ops(model.boxed())),
        Box::new(CloneOdooRepo::with_ops(model.boxed())),
        Box::new(CreateVirtualenv::with_ops(model.boxed())),
        Box::new(GenerateConfig::with_ops(model.boxed())),
        Box::new(InitializeOdooDatabase::with_ops(model.boxed())),
        Box::new(SetupSystemd::with_ops(model.boxed())),
        Box::new(WriteControlScript::with_ops(model.boxed())),
        Box::new(PatchBashrc::with_ops(model.boxed())),
    ]
}

use odoo_installer::steps::setup_postgres::SetupPostgres;

#[test]
fn full_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // Sequenza completa + uno step che fallisce alla fine → rollback di tutto.
    let mut steps = chain(&model);
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), /* aggressive */ true);
    let mut installer = Installer::new();
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "il fallimento innesca il rollback"
    );

    assert_eq!(
        model.snapshot(),
        initial,
        "dopo il rollback il sistema è tornato al vergine"
    );
    // Il .bashrc dell'utente è byte-per-byte come prima.
    assert_eq!(
        model
            .snapshot()
            .file_contents
            .get(&PathBuf::from(BASHRC))
            .map(String::as_str),
        Some(BASHRC_ORIG)
    );
}

#[test]
fn mid_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // Fallimento "a CloneOdooRepo": i primi 5 step (utente, deps, postgres,
    // ruolo, DB) completano, poi si inietta il fallimento.
    let mut steps: Vec<Box<dyn Step>> = chain(&model).into_iter().take(5).collect();
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    assert_eq!(
        model.snapshot(),
        initial,
        "utente/deps/postgres/ruolo/DB creati devono sparire"
    );
}

#[test]
fn preexisting_resources_survive_rollback() {
    // Stato iniziale con RISORSE DEL CLIENTE: PostgreSQL installato+attivo, un DB
    // 'odoo' che esiste già (dati del cliente!). L'init hard-stoppa (DB non
    // nostro), la catena fallisce, e il rollback NON tocca ciò che era già lì.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state();
    init.packages.insert("postgresql".to_string());
    init.packages.insert("postgresql-contrib".to_string());
    init.svc_enabled.insert("postgresql".to_string());
    init.svc_active.insert("postgresql".to_string());
    init.pg_dbs.insert("odoo".to_string()); // DB preesistente del cliente

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain(&model);
    let ctx = ctx(dir.path().join("state.json"), false);
    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);
    assert!(
        result.is_err(),
        "l'hard-stop init deve fermare la catena su DB preesistente"
    );

    let final_state = model.snapshot();
    // Tutto tornato com'era.
    assert_eq!(
        final_state, initial,
        "le risorse preesistenti restano, le nostre spariscono"
    );
    // Verifiche esplicite delle protezioni critiche a livello di catena:
    assert!(
        final_state.pg_dbs.contains("odoo"),
        "il DB del cliente NON deve essere droppato"
    );
    assert!(
        final_state.packages.contains("postgresql"),
        "PostgreSQL preinstallato resta"
    );
    assert!(
        final_state.svc_active.contains("postgresql"),
        "il servizio già attivo resta attivo (D4)"
    );
    assert!(
        final_state.paths.contains(&PathBuf::from(HOME)),
        "/opt/odoo preesistente resta"
    );
}

// --- Nginx nella catena completa (R3, audit A4.2) ---------------------------
//
// I 5 step Nginx avevano solo unit test isolati. Qui girano nella catena reale,
// nella posizione reale, e il loro rollback si intreccia con quello degli altri
// step nell'ordine inverso. È dove vivono le mutazioni su config di **terzi**:
// il default site del cliente e le regole del suo firewall.

const VHOST: &str = "/etc/nginx/sites-available/odoo18";
const SITE_LINK: &str = "/etc/nginx/sites-enabled/odoo18";
const DEFAULT_SITE: &str = "/etc/nginx/sites-enabled/default";

/// Step che **fotografa** il modello a metà catena e poi fallisce, innescando
/// il rollback.
///
/// Serve a rendere le asserzioni oneste: verificare solo lo stato *finale* non
/// distingue "mutato e poi ripristinato" da "mai toccato". Un default site che
/// c'è ancora alla fine potrebbe non essere mai stato rimosso; una regola ufw
/// assente alla fine potrebbe non essere mai stata aperta. La foto intermedia
/// prova il primo tempo, lo stato finale prova il secondo.
struct SpyThenFail {
    model: SystemModel,
    seen: std::sync::Arc<std::sync::Mutex<Option<ModelState>>>,
}

impl SpyThenFail {
    #[allow(clippy::type_complexity)]
    fn new(model: &SystemModel) -> (Self, std::sync::Arc<std::sync::Mutex<Option<ModelState>>>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        (
            SpyThenFail {
                model: model.handle(),
                seen: std::sync::Arc::clone(&seen),
            },
            seen,
        )
    }
}

impl Step for SpyThenFail {
    fn name(&self) -> &str {
        "spy-then-fail"
    }
    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn run(&mut self, _ctx: &Context) -> Result<(), StepError> {
        if let Ok(mut slot) = self.seen.lock() {
            *slot = Some(self.model.snapshot());
        }
        Err(StepError::Precondition(
            "fallimento iniettato a valle della fase Nginx".to_string(),
        ))
    }
    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// Legge la foto scattata da [`SpyThenFail`].
fn photo(slot: &std::sync::Arc<std::sync::Mutex<Option<ModelState>>>) -> ModelState {
    slot.lock()
        .ok()
        .and_then(|s| s.clone())
        .expect("lo step spia deve aver girato")
}

/// Come [`ctx`], ma con la fase Nginx attiva.
fn ctx_nginx(state_path: PathBuf, aggressive: bool, ssl: bool) -> Context {
    Context {
        with_nginx: true,
        nginx_server_name: "_".to_string(),
        nginx_open_https_port: ssl,
        ..ctx(state_path, aggressive)
    }
}

/// Stato iniziale con ufw installato **e attivo**: senza, `NginxFirewall` è un
/// no-op dichiarato e il delta non verrebbe mai esercitato.
fn fresh_state_with_ufw() -> ModelState {
    ModelState {
        ufw_available: true,
        ufw_active: true,
        ..fresh_state()
    }
}

/// La catena reale **inclusi** i 5 step Nginx, nella posizione di `main.rs`:
/// dopo `setup-systemd`, prima del control-script.
fn chain_with_nginx(model: &SystemModel) -> Vec<Box<dyn Step>> {
    let mut steps = chain(model);
    let at = steps
        .iter()
        .position(|s| s.name() == "setup-systemd")
        .map(|i| i + 1)
        .unwrap_or(steps.len());
    let nginx: Vec<Box<dyn Step>> = vec![
        Box::new(NginxInstall::with_ops(model.boxed())),
        Box::new(NginxWriteConfig::with_ops(model.boxed())),
        Box::new(NginxEnableSite::with_ops(model.boxed())),
        Box::new(NginxFirewall::with_ops(model.boxed())),
        Box::new(NginxReload::with_ops(model.boxed())),
    ];
    steps.splice(at..at, nginx);
    steps
}

#[test]
fn full_chain_with_nginx_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state_with_ufw());
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let ctx = ctx_nginx(dir.path().join("state.json"), true, /* ssl */ false);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // Primo tempo: la fase Nginx aveva davvero mutato il sistema.
    let mid = photo(&seen);
    assert!(mid.packages.contains("nginx"), "nginx installato");
    assert!(mid.paths.contains(&PathBuf::from(VHOST)), "vhost scritto");
    assert!(
        mid.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "sito abilitato"
    );
    assert!(mid.ufw_rules.contains("80/tcp"), "porta 80 aperta");
    assert!(mid.svc_active.contains("nginx"), "nginx avviato");

    // Secondo tempo: il rollback ha annullato tutto.
    let final_state = model.snapshot();
    assert_eq!(
        final_state, initial,
        "col rollback anche la fase Nginx torna al vergine"
    );
    // Verifiche esplicite sui singoli artefatti Nginx (un `assert_eq` che
    // fallisce non direbbe *quale* è rimasto).
    assert!(
        !final_state.packages.contains("nginx"),
        "nginx installato da noi va purgato con --aggressive-rollback"
    );
    assert!(!final_state.paths.contains(&PathBuf::from(VHOST)), "vhost");
    assert!(
        !final_state.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "symlink sites-enabled"
    );
    assert!(final_state.ufw_rules.is_empty(), "regole ufw del delta");
    assert!(
        !final_state.svc_active.contains("nginx") && !final_state.svc_enabled.contains("nginx"),
        "servizio nginx avviato/abilitato da noi va fermato e disabilitato"
    );
}

#[test]
fn preexisting_default_site_is_restored_by_the_full_chain_rollback() {
    // La mutazione più insidiosa della fase: per liberare la 80 rimuoviamo il
    // default site, che è **config del cliente**. Provato finora solo in
    // isolamento; qui il ripristino avviene in fondo a un rollback completo.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    // Cliente con nginx già installato, attivo, e il suo default site.
    init.packages.insert("nginx".to_string());
    init.svc_enabled.insert("nginx".to_string());
    init.svc_active.insert("nginx".to_string());
    init.symlinks.insert(PathBuf::from(DEFAULT_SITE));
    // Nginx gira e sta servendo il suo default site: la config **caricata**
    // coincide con quella su disco.
    init.nginx_loaded_sites = Some([PathBuf::from(DEFAULT_SITE)].into_iter().collect());

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    // `aggressive` = true di proposito: nemmeno il rollback più aggressivo deve
    // toccare un nginx che era già del cliente.
    let ctx = ctx_nginx(dir.path().join("state.json"), true, false);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // Primo tempo: il default site è stato davvero rimosso per liberare la 80,
    // e il nostro sito abilitato al suo posto.
    let mid = photo(&seen);
    assert!(
        !mid.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "durante l'installazione il default site va rimosso (porta 80 liberata)"
    );
    assert!(
        mid.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "il nostro sito è abilitato"
    );

    // Secondo tempo: il rollback ha ricucito la config del cliente.
    let final_state = model.snapshot();
    assert!(
        final_state.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "il default site del cliente deve essere ripristinato dal rollback"
    );
    assert!(
        final_state.packages.contains("nginx"),
        "nginx preesistente NON va purgato, nemmeno con --aggressive-rollback"
    );
    assert!(
        final_state.svc_active.contains("nginx") && final_state.svc_enabled.contains("nginx"),
        "un nginx già attivo/abilitato resta tale (D4)"
    );
    // Non basta rimettere a posto i **file**: nginx continua a servire la config
    // che ha in memoria. Se il rollback lo lascia con la nostra config caricata,
    // il sito del cliente resta giù finché qualcuno non ricarica a mano — un
    // rollback "completo" sulla carta e rotto in produzione.
    assert_eq!(
        final_state.nginx_loaded_sites,
        Some([PathBuf::from(DEFAULT_SITE)].into_iter().collect()),
        "dopo il rollback nginx deve **servire** la config ripristinata, non la nostra"
    );
    assert_eq!(final_state, initial, "stato finale == stato iniziale");
}

#[test]
fn only_our_ufw_delta_is_removed_by_the_full_chain_rollback() {
    // Il cliente aveva già aperto la 80; noi apriamo la 443. Il rollback deve
    // rimuovere **solo** la 443: la 80 non è nel nostro delta.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    init.ufw_rules.insert("80/tcp".to_string());

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let ctx = ctx_nginx(dir.path().join("state.json"), true, /* ssl */ true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // Primo tempo: la 443 è stata davvero aperta da noi (la 80 c'era già).
    let mid = photo(&seen);
    assert!(
        mid.ufw_rules.contains("443/tcp"),
        "la regola del delta viene aperta durante l'installazione"
    );

    // Secondo tempo: il rollback rimuove solo ciò che ha aperto lui.
    let final_state = model.snapshot();
    assert!(
        final_state.ufw_rules.contains("80/tcp"),
        "la regola preesistente del cliente resta aperta"
    );
    assert!(
        !final_state.ufw_rules.contains("443/tcp"),
        "la regola aggiunta da noi (delta) viene rimossa"
    );
    assert_eq!(final_state, initial, "stato finale == stato iniziale");
}

#[test]
fn full_chain_with_nginx_succeeds_and_leaves_the_expected_artifacts() {
    // Il lato **run** della fase Nginx nella catena reale: finora nessun test
    // e2e arrivava in fondo senza fallimenti iniettati.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    init.symlinks.insert(PathBuf::from(DEFAULT_SITE));
    let model = SystemModel::new(init);

    let mut steps = chain_with_nginx(&model);
    let ctx = ctx_nginx(dir.path().join("state.json"), false, /* ssl */ true);
    let mut installer = Installer::new();
    installer
        .execute(&mut steps, &ctx)
        .expect("la catena completa con nginx deve arrivare in fondo");

    let s = model.snapshot();
    assert!(s.packages.contains("nginx"));
    assert!(s.paths.contains(&PathBuf::from(VHOST)), "vhost scritto");
    assert!(
        s.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "sito abilitato"
    );
    assert!(s.svc_active.contains("nginx"), "nginx avviato");
    assert!(s.ufw_rules.contains("80/tcp") && s.ufw_rules.contains("443/tcp"));
    assert!(
        !s.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "a installazione riuscita il default site resta rimosso: è la porta 80 \
         che serve al nostro vhost"
    );
}

#[test]
fn nginx_steps_are_inert_in_the_full_chain_without_with_nginx() {
    // Gating nella catena completa: gli stessi 5 step, con `with_nginx = false`,
    // non devono lasciare **nessuna** traccia dopo un'installazione riuscita.
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state_with_ufw());

    let mut steps = chain_with_nginx(&model);
    let ctx = ctx(dir.path().join("state.json"), false); // with_nginx = false
    let mut installer = Installer::new();
    installer
        .execute(&mut steps, &ctx)
        .expect("catena senza nginx");

    let s = model.snapshot();
    assert!(!s.packages.contains("nginx"), "nessun pacchetto nginx");
    assert!(!s.paths.contains(&PathBuf::from(VHOST)), "nessun vhost");
    assert!(
        !s.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "nessun sito abilitato"
    );
    assert!(s.ufw_rules.is_empty(), "nessuna regola firewall");
    assert!(!s.svc_enabled.contains("nginx") && !s.svc_active.contains("nginx"));
}

// --- Regressione dal campo: dpkg rotto (A-RT-1 / A-RT-2) --------------------
//
// Scenario reale osservato su Multipass (Ubuntu 22.04 pulita): il `.deb` di
// wkhtmltopdf ha dipendenze di sistema assenti sulla VM, `dpkg -i` fallisce
// lasciando dpkg inconsistente, la catena si ferma — e il rollback non riesce a
// purgare i 24 pacchetti del delta perché apt rifiuta di operare su un dpkg
// rotto. Il sistema resta sporco: la promessa chirurgica violata proprio nello
// scenario in cui serve di più.

/// Le dipendenze di sistema del `.deb` di wkhtmltopdf su una VM minimale.
const WK_SYSTEM_DEPS: &[&str] = &["fontconfig", "libxrender1", "xfonts-75dpi", "xfonts-base"];

/// Step di test che rompe `dpkg` e poi fallisce: modella "uno step a valle ha
/// lasciato il sistema in stato inconsistente prima che partisse il rollback".
struct BreakDpkgThenFail {
    model: SystemModel,
}

impl Step for BreakDpkgThenFail {
    fn name(&self) -> &str {
        "break-dpkg"
    }
    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn run(&mut self, _ctx: &Context) -> Result<(), StepError> {
        self.model.mutate(|s| s.dpkg_broken = true);
        Err(StepError::Precondition(
            "step fallito lasciando dpkg in stato inconsistente".to_string(),
        ))
    }
    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

#[test]
fn rollback_cleans_the_apt_delta_even_with_a_broken_dpkg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // Catena reale: i pacchetti di sistema vengono installati, poi uno step a
    // valle muore lasciando dpkg rotto.
    let mut steps = chain(&model);
    steps.push(Box::new(BreakDpkgThenFail {
        model: model.handle(),
    }));

    let ctx = ctx(dir.path().join("state.json"), true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    let final_state = model.snapshot();
    // Il cuore della regressione: prima del fix questi 24+ pacchetti restavano
    // installati perché `apt-get purge` falliva su un dpkg rotto.
    for pkg in ["build-essential", "python3-dev", "libpq-dev"] {
        assert!(
            !final_state.packages.contains(pkg),
            "'{pkg}' del delta doveva essere purgato: il rollback deve recuperare \
             dpkg prima di arrendersi"
        );
    }
    assert!(
        !final_state.dpkg_broken,
        "il rollback deve lasciare dpkg in stato consistente"
    );
    assert_eq!(
        final_state, initial,
        "anche partendo da dpkg rotto il sistema torna al vergine"
    );
}

// --- wkhtmltopdf nella catena completa (R3, audit A4.2) ---------------------

#[test]
fn wkhtmltopdf_is_installed_in_the_chain_and_purged_by_the_rollback() {
    // R0 ha coperto download → verifica → install a livello di step, ma **non**
    // l'undo: qui il ramo felice gira nella catena e il rollback deve riportare
    // il sistema a "wkhtmltopdf assente".
    let dir = tempfile::tempdir().expect("tempdir");
    // VM minimale come quella di prova: le dipendenze di sistema del `.deb` non
    // ci sono e vanno risolte dall'installazione (A-RT-1).
    let mut init = fresh_state();
    init.pending_deps = WK_SYSTEM_DEPS.iter().map(|s| s.to_string()).collect();
    let model = SystemModel::new(init);
    let initial = model.snapshot();
    assert!(initial.wk_version.is_none(), "si parte senza wkhtmltopdf");

    // `.deb` finto il cui hash è, per costruzione, quello atteso dalla tabella.
    let bytes = b"contenuto .deb di prova".to_vec();
    let probe = dir.path().join("probe.bin");
    std::fs::write(&probe, &bytes).expect("probe");
    let sha = odoo_installer::system_ops::sha256_hex(&probe).expect("hash");
    let checksums = [("jammy".to_string(), sha)].into_iter().collect();

    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let wk = InstallWkhtmltopdf::with_parts(
        model.boxed(),
        Box::new(MockDownloader::new(bytes, log)),
        checksums,
        dir.path().to_path_buf(),
    );

    // Posizione reale: subito dopo le dipendenze apt.
    let mut steps = chain(&model);
    let at = steps
        .iter()
        .position(|s| s.name() == "apt-packages")
        .map(|i| i + 1)
        .unwrap_or(0);
    steps.insert(at, Box::new(wk));
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let mut c = ctx(dir.path().join("state.json"), true);
    c.os_info = Some(OsInfo {
        id: "ubuntu".to_string(),
        version: "22.04".to_string(),
        codename: Some("jammy".to_string()),
        family: odoo_installer::distro::OsFamily::Debian,
    });

    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &c).is_err());

    // Primo tempo: il ramo felice è arrivato fino a `dpkg -i` (se il checksum
    // non fosse combaciato lo step avrebbe fallito prima).
    let mid = photo(&seen);
    assert!(
        mid.packages.contains("wkhtmltox"),
        "download → checksum verificato → installato"
    );
    assert_eq!(mid.wk_version.as_deref(), Some("0.12.6.1"));
    // A-RT-1: le dipendenze di sistema del `.deb` sono state risolte, non
    // ignorate. Con `dpkg -i` lo step sarebbe fallito qui.
    for dep in WK_SYSTEM_DEPS {
        assert!(
            mid.packages.contains(*dep),
            "'{dep}' doveva essere installato come dipendenza del .deb"
        );
    }

    // Secondo tempo: il rollback purga wkhtmltox — ma **non** le sue dipendenze
    // di sistema, che restano come git/curl del bootstrap (D3).
    let final_state = model.snapshot();
    assert!(
        !final_state.packages.contains("wkhtmltox"),
        "wkhtmltopdf installato da noi va purgato nel rollback"
    );
    assert!(final_state.wk_version.is_none());
    for dep in WK_SYSTEM_DEPS {
        assert!(
            final_state.packages.contains(*dep),
            "'{dep}' è una libreria di sistema: il rollback la lascia (D3)"
        );
    }
}
