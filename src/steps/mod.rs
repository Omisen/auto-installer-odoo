//! Gli step dell'installer.
//!
//! Ogni step vive in un proprio file: le fasi successive aggiungono un modulo
//! qui senza toccare il motore né gli altri step.
//!
//! Il modulo espone anche i due punti d'accesso alla sequenza:
//! [`build_steps`] (l'ordine canonico dell'installazione) e [`step_by_name`]
//! (la ricostruzione di un singolo step dal suo nome, usata dal rollback da
//! disco). I due devono coprire lo stesso insieme di step: un test lo verifica.

use tracing::{info, warn};

use crate::step::Step;
use crate::system_ops::{Downloader, RealDownloader, RealSystemOps, SystemOps};

/// Purga pacchetti in un `undo`, **recuperando `dpkg`** se è in stato
/// inconsistente. Best-effort: non fallisce mai, logga cosa resta.
///
/// # Perché serve (A-RT-2, trovato in campo)
///
/// Il rollback arriva sempre *dopo* un fallimento, e quel fallimento può aver
/// lasciato `dpkg` a metà. In quello stato apt si rifiuta di operare
/// (`E: Unmet dependencies. Try 'apt --fix-broken install'`) e **ogni** purge
/// del rollback fallisce: i pacchetti che avevamo installato restano lì e il
/// sistema non torna pulito — la promessa chirurgica violata proprio nello
/// scenario in cui serve di più. Sulla VM di prova restarono 24 pacchetti.
///
/// Perciò: `apt-get install -f` prima del purge (no-op innocuo se `dpkg` è già
/// sano) e, se il purge fallisce lo stesso, un secondo tentativo dopo un
/// `dpkg --configure -a`. Se nemmeno così funziona si prosegue — l'undo è
/// best-effort (invariante 3) — ma si elenca **esattamente** cosa è rimasto,
/// perché è materiale che l'utente dovrà rimuovere a mano.
///
/// Vive qui e non in `SystemOps` perché è **politica di rollback**, non un
/// comando di sistema: il confine resta una mappatura 1:1 sui comandi, e i
/// test possono asserire la sequenza esatta.
pub fn purge_with_dpkg_recovery(ops: &dyn SystemOps, step: &str, pkgs: &[&str]) {
    if pkgs.is_empty() {
        return;
    }

    // Recovery preventivo: apt non opera su un dpkg rotto.
    if let Err(e) = ops.apt_fix_broken() {
        warn!(step, error = %e, "undo: fix-broken preventivo fallito, tento comunque il purge");
    }

    let Err(first) = ops.apt_purge(pkgs) else {
        return;
    };
    warn!(step, error = %first, "undo: purge fallito, tento il recovery di dpkg e riprovo");

    // `dpkg --configure -a` copre i pacchetti scompattati ma non configurati,
    // dove `apt-get install -f` da solo non basta.
    if let Err(e) = ops.dpkg_configure_all() {
        warn!(step, error = %e, "undo: dpkg --configure -a fallito");
    }
    if let Err(e) = ops.apt_fix_broken() {
        warn!(step, error = %e, "undo: fix-broken di recovery fallito");
    }

    match ops.apt_purge(pkgs) {
        Ok(()) => info!(step, "undo: purge riuscito dopo il recovery di dpkg"),
        Err(second) => warn!(
            step,
            error = %second,
            residui = ?pkgs,
            "undo: purge fallito anche dopo il recovery di dpkg, proseguo (best-effort). \
             Questi pacchetti restano installati: rimuovili a mano dopo aver sistemato dpkg \
             (`sudo apt-get install -f`)"
        ),
    }
}

/// Il primo livello **inesistente** scendendo da `home` verso `target`: la
/// radice di ciò che un `mkdir -p` creerà, e quindi l'unica cosa che un undo può
/// rimuovere senza toccare roba di altri.
///
/// `None` se `target` non è sotto `home` o se esiste già tutto.
///
/// Vive qui perché la usano due step — il filestore
/// ([`setup_data_dir`]) e la cache ([`setup_cache_dir`]) — e sono entrambi casi
/// dello stesso problema: creare una sottodirectory dentro la home dell'utente
/// `odoo`, che è `Preexisting` e non va svuotata, sapendo *esattamente* quanto
/// di quell'albero abbiamo aggiunto noi.
pub fn highest_missing_level(
    ops: &dyn SystemOps,
    home: &std::path::Path,
    target: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let relative = target.strip_prefix(home).ok()?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        current = current.join(component);
        if !ops.path_exists(&current) {
            return Some(current);
        }
    }
    None
}

/// Rimuove ricorsivamente `created_root`, con la **rete sul perimetro**.
///
/// L'undo di questi step cancella un albero a partire da un path che arriva
/// **dal disco** (lo snapshot persistito). Uno stato corrotto — o scritto da
/// un'altra installazione — non deve poter diventare un `rm -rf` altrove:
/// il target dev'essere un discendente *stretto* di `home`, altrimenti si logga
/// e non si tocca nulla. Meglio un residuo da rimuovere a mano.
///
/// Best-effort come ogni undo (invariante 3): un fallimento è un `warn!`, non un
/// errore che ferma la pulizia degli altri step.
pub fn remove_created_root(
    ops: &dyn SystemOps,
    step: &str,
    home: &std::path::Path,
    target: &std::path::Path,
    dry_run: bool,
) {
    if !target.starts_with(home) || target == home {
        warn!(
            step,
            target = %target.display(),
            home = %home.display(),
            "undo: path fuori dal perimetro della home, non rimuovo nulla"
        );
        return;
    }
    if dry_run {
        info!(step, target = %target.display(), "undo (dry-run): rm -rf");
        return;
    }
    match ops.remove_dir_all(target) {
        Ok(()) => info!(step, target = %target.display(), "undo: rimosso"),
        Err(e) => warn!(
            step,
            target = %target.display(),
            error = %e,
            "undo: rm -rf fallito, proseguo (best-effort)"
        ),
    }
}

/// Fabbrica di [`SystemOps`]: ogni step ne riceve una propria istanza.
///
/// È un `Fn` e non un singolo `Box<dyn SystemOps>` perché gli step possiedono le
/// proprie `ops`: servono N istanze, non N riferimenti. In produzione ritorna
/// `RealSystemOps`; nei test ritorna handle allo stesso `SystemModel`, così le
/// mutazioni di uno step sono viste dagli undo degli altri.
pub type OpsFactory<'a> = &'a dyn Fn() -> Box<dyn SystemOps>;

/// Costruisce la sequenza di produzione degli step, **nell'ordine di
/// esecuzione**. Il rollback li annulla in ordine inverso (invariante 2).
///
/// Vive qui e non in `main` perché è la definizione canonica della sequenza:
/// [`step_by_name`] deve coprirla per intero perché il rollback da disco
/// funzioni, e un test di parità lo verifica — aggiungere uno step qui senza
/// aggiungerlo là fa fallire la build dei test, non un rollback su una macchina
/// cliente.
pub fn build_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(prepare_opt_root::PrepareOptRoot::new()),
        Box::new(create_odoo_user::CreateOdooUser::new()),
        Box::new(setup_log_dir::SetupLogDir::new()),
        // Presto di proposito: lo snapshot deve vedere la home PRIMA che
        // qualunque programma lanciato come `odoo` ci scriva una cache. Essere
        // presto qui significa essere tardi nell'undo, che è dove serve
        // (A-R5-3).
        Box::new(setup_cache_dir::SetupCacheDir::new()),
        Box::new(apt_packages::AptPackagesStep::bootstrap()),
        Box::new(apt_packages::AptPackagesStep::odoo_dependencies()),
        Box::new(install_wkhtmltopdf::InstallWkhtmltopdf::new()),
        // PostgreSQL: undo inverso CreateDatabase → CreateDbRole → SetupPostgres.
        Box::new(setup_postgres::SetupPostgres::new()),
        Box::new(create_db_role::CreateDbRole::new()),
        Box::new(create_database::CreateDatabase::new()),
        // Sorgenti: clone → venv → pip (undo pip no-op; venv rm; clone rm).
        Box::new(clone_odoo_repo::CloneOdooRepo::new()),
        Box::new(create_virtualenv::CreateVirtualenv::new()),
        Box::new(install_python_requirements::InstallPythonRequirements::new()),
        // Config + init schema (undo init no-op: pulizia dal dropdb di Fase 5).
        Box::new(generate_config::GenerateConfig::new()),
        // Il filestore va creato prima che Odoo lo crei da sé: solo così è un
        // artefatto registrato, e quindi annullabile (A-R5-3). Deve stare dopo
        // `create-database`, da cui legge se il DB è nostro.
        Box::new(setup_data_dir::SetupDataDir::new()),
        Box::new(initialize_odoo_database::InitializeOdooDatabase::new()),
        // Servizio systemd (undo: stop → disable → rm → daemon-reload).
        Box::new(setup_systemd::SetupSystemd::new()),
        // Nginx (opzionale, gated): install → vhost → enable → firewall → reload.
        Box::new(nginx_install::NginxInstall::new()),
        Box::new(nginx_write_config::NginxWriteConfig::new()),
        Box::new(nginx_enable_site::NginxEnableSite::new()),
        Box::new(nginx_firewall::NginxFirewall::new()),
        Box::new(nginx_reload::NginxReload::new()),
        // Comando helper `odoo` + patch PATH nel .bashrc dell'utente.
        Box::new(write_control_script::WriteControlScript::new()),
        Box::new(patch_bashrc::PatchBashrc::new()),
    ]
}

/// Ricostruisce uno step dal suo **nome persistito**, con `SystemOps` iniettabili.
///
/// È la metà "identità" della reidratazione: dato uno `StepRecord`,
/// `step_by_name` produce l'oggetto e [`crate::step::Step::rehydrate`] ne
/// rimette lo stato. Insieme rendono eseguibile l'`undo` di un'installazione
/// che questo processo non ha mai eseguito.
///
/// Ritorna `None` per un nome sconosciuto — uno stato scritto da una versione
/// con step che qui non esistono più. Il chiamante lo tratta come residuo da
/// segnalare, non come errore fatale: gli altri step vanno comunque annullati.
///
/// Il downloader di `install-wkhtmltopdf` è sempre quello reale: l'undo purga un
/// pacchetto, non scarica nulla, quindi non c'è nulla da iniettare. Stessa
/// ragione per la tabella dei checksum, presa dai pin di produzione.
pub fn step_by_name(name: &str, make_ops: OpsFactory<'_>) -> Option<Box<dyn Step>> {
    let step: Box<dyn Step> = match name {
        "prepare-opt-root" => Box::new(prepare_opt_root::PrepareOptRoot::new()),
        "create-odoo-user" => Box::new(create_odoo_user::CreateOdooUser::with_ops(make_ops())),
        "setup-log-dir" => Box::new(setup_log_dir::SetupLogDir::with_ops(make_ops())),
        "setup-cache-dir" => Box::new(setup_cache_dir::SetupCacheDir::with_ops(make_ops())),
        "bootstrap-prerequisites" => {
            Box::new(apt_packages::AptPackagesStep::bootstrap_with_ops(make_ops()))
        }
        "install-system-dependencies" => Box::new(
            apt_packages::AptPackagesStep::odoo_dependencies_with_ops(make_ops()),
        ),
        "install-wkhtmltopdf" => Box::new(install_wkhtmltopdf::InstallWkhtmltopdf::with_parts(
            make_ops(),
            Box::new(RealDownloader::new()) as Box<dyn Downloader>,
            install_wkhtmltopdf::default_checksums(),
            std::env::temp_dir(),
        )),
        "setup-postgres" => Box::new(setup_postgres::SetupPostgres::with_ops(make_ops())),
        "create-db-role" => Box::new(create_db_role::CreateDbRole::with_ops(make_ops())),
        "create-database" => Box::new(create_database::CreateDatabase::with_ops(make_ops())),
        "clone-odoo-repo" => Box::new(clone_odoo_repo::CloneOdooRepo::with_ops(make_ops())),
        "create-virtualenv" => Box::new(create_virtualenv::CreateVirtualenv::with_ops(make_ops())),
        "install-python-requirements" => Box::new(
            install_python_requirements::InstallPythonRequirements::with_parts(
                make_ops(),
                std::env::temp_dir(),
            ),
        ),
        "generate-config" => Box::new(generate_config::GenerateConfig::with_ops(make_ops())),
        "setup-data-dir" => Box::new(setup_data_dir::SetupDataDir::with_ops(make_ops())),
        "initialize-odoo-database" => Box::new(
            initialize_odoo_database::InitializeOdooDatabase::with_ops(make_ops()),
        ),
        "setup-systemd" => Box::new(setup_systemd::SetupSystemd::with_ops(make_ops())),
        "nginx-install" => Box::new(nginx_install::NginxInstall::with_ops(make_ops())),
        "nginx-write-config" => {
            Box::new(nginx_write_config::NginxWriteConfig::with_ops(make_ops()))
        }
        "nginx-enable-site" => Box::new(nginx_enable_site::NginxEnableSite::with_ops(make_ops())),
        "nginx-firewall" => Box::new(nginx_firewall::NginxFirewall::with_ops(make_ops())),
        "nginx-reload" => Box::new(nginx_reload::NginxReload::with_ops(make_ops())),
        "write-control-script" => Box::new(write_control_script::WriteControlScript::with_ops(
            make_ops(),
        )),
        "patch-bashrc" => Box::new(patch_bashrc::PatchBashrc::with_ops(make_ops())),
        _ => return None,
    };
    Some(step)
}

/// Fabbrica di produzione: [`RealSystemOps`] per ogni step.
pub fn real_ops() -> Box<dyn SystemOps> {
    Box::new(RealSystemOps::new())
}

/// I nomi degli step della sequenza canonica, nell'ordine di esecuzione.
///
/// Derivato da [`build_steps`] invece che scritto a mano: una lista parallela
/// diventerebbe stantìa senza che nulla se ne accorga, e qui il costo è un giro
/// di costruttori senza effetti collaterali.
pub fn canonical_step_names() -> Vec<String> {
    build_steps().iter().map(|s| s.name().to_string()).collect()
}

pub mod apt_packages;
pub mod clone_odoo_repo;
pub mod create_database;
pub mod create_db_role;
pub mod create_odoo_user;
pub mod create_virtualenv;
pub mod generate_config;
pub mod initialize_odoo_database;
pub mod install_python_requirements;
pub mod install_wkhtmltopdf;
pub mod nginx_enable_site;
pub mod nginx_firewall;
pub mod nginx_install;
pub mod nginx_reload;
pub mod nginx_write_config;
pub mod noop;
pub mod patch_bashrc;
pub mod prepare_opt_root;
pub mod setup_cache_dir;
pub mod setup_data_dir;
pub mod setup_log_dir;
pub mod setup_postgres;
pub mod setup_systemd;
pub mod write_control_script;
