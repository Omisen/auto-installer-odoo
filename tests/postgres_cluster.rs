//! M3 — l'inizializzazione del cluster PostgreSQL, quarto asse di
//! `setup-postgres`.
//!
//! # La divergenza più pesante fra le due famiglie
//!
//! Su Debian/Ubuntu il postinst di `postgresql` chiama `pg_createcluster` e il
//! servizio parte: `setup-postgres` non ha mai avuto un passo di
//! inizializzazione perché non serviva. Su Fedora `postgresql-server` **non
//! inizializza niente** — senza `postgresql-setup --initdb` il servizio non
//! parte, e lo step falliva alla verifica finale con un messaggio che mandava a
//! leggere `journalctl` invece di dire la causa.
//!
//! Non è «un comando in più»: l'init **produce un artefatto**, il data
//! directory, che senza un `PreState` proprio nascerebbe senza che nessuno lo
//! annoti — cioè non sarebbe annullabile (A-R5-3).

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::OsFamily;
use invok::error::StepError;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::setup_postgres::{PostgresSnapshot, SetupPostgres};

fn ctx(aggressive: bool) -> Context {
    Context {
        dry_run: false,
        aggressive_rollback: aggressive,
        db_name: "odoo".to_string(),
        ..Default::default()
    }
}

fn esegui(cfg: MockConfig) -> (SetupPostgres, common::OpLog) {
    let (ops, log) = MockSystemOps::new(cfg);
    let mut step = SetupPostgres::with_ops(Box::new(ops));
    step.snapshot(&ctx(false)).expect("snapshot");
    step.run(&ctx(false)).expect("run");
    (step, log)
}

fn snap(step: &SetupPostgres) -> PostgresSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

// --- Il cluster si inizializza solo dove serve -------------------------------

/// Su Fedora il cluster va creato, e l'operazione è **registrata**.
#[test]
fn on_fedora_the_cluster_is_initialized_and_recorded() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });

    assert!(
        ops_of(&log).contains(&Op::InitPostgresCluster),
        "senza initdb il servizio non parte: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        snap(&step).cluster_initialized,
        PreState::CreatedByUs,
        "l'abbiamo creato noi: è un artefatto nostro, e l'undo deve saperlo"
    );
}

/// Su Debian **non** si inizializza nulla: il pacchetto lo fa da sé, e un initdb
/// in più su un cluster già esistente non è innocuo.
#[test]
fn on_debian_nothing_is_initialized() {
    let (step, log) = esegui(MockConfig::default());

    assert!(
        !ops_of(&log).contains(&Op::InitPostgresCluster),
        "su questa famiglia il cluster lo crea il postinst del pacchetto"
    );
    assert_eq!(
        snap(&step).cluster_initialized,
        PreState::Untracked,
        "niente da inizializzare = niente da annullare"
    );
}

/// Un cluster **già inizializzato** non si tocca, nemmeno su Fedora: è
/// `Preexisting`, e reinizializzarlo distruggerebbe i database che ospita.
#[test]
fn an_existing_cluster_is_left_alone() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_cluster_initialized: true,
        ..MockConfig::default()
    });

    assert!(
        !ops_of(&log).contains(&Op::InitPostgresCluster),
        "il cluster c'era già: `postgresql-setup --initdb` su un PGDATA popolato \
         è esattamente ciò che non deve succedere"
    );
    assert_eq!(snap(&step).cluster_initialized, PreState::Preexisting);
}

/// L'init avviene **prima** di enable e start.
///
/// L'ordine non è cosmetico: è tutta la ragione per cui questo è un asse di
/// `setup-postgres` e non uno step a sé. Su Fedora un `systemctl start` prima
/// dell'initdb fallisce, e lo step si fermerebbe alla verifica finale dicendo di
/// guardare `journalctl` — cioè indicando il sintomo invece della causa.
#[test]
fn the_cluster_is_initialized_before_the_service_starts() {
    let (_step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });

    let ops = ops_of(&log);
    let posizione = |cerca: fn(&Op) -> bool| ops.iter().position(cerca);

    let init = posizione(|op| matches!(op, Op::InitPostgresCluster)).expect("init eseguito");
    let install = posizione(|op| matches!(op, Op::PkgInstall(_))).expect("install eseguito");
    let start = posizione(|op| matches!(op, Op::ServiceStart(_))).expect("start eseguito");

    assert!(
        install < init,
        "prima si installa il pacchetto, poi si inizializza il cluster: {ops:?}"
    );
    assert!(
        init < start,
        "l'initdb deve precedere lo start, o il servizio non parte: {ops:?}"
    );
}

// --- L'undo: il cluster segue la politica del pacchetto ----------------------

/// **Senza `--aggressive-rollback` il cluster resta.**
///
/// Un data directory vuoto è un residuo inerte; i dati di qualcun altro no. È la
/// stessa asimmetria di D3-punto2 per il purge del pacchetto: stop e disable
/// sono reversibili, la rimozione dei dati non lo è.
#[test]
fn without_the_aggressive_flag_the_cluster_survives() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    step.undo(&ctx(false)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "il cluster non si rimuove senza flag esplicito: {:?}",
        ops_of(&log)
    );
}

/// Con il flag **e** con il cluster vuoto di database altrui, si rimuove.
#[test]
fn with_the_aggressive_flag_and_an_empty_cluster_it_is_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        // Solo il nostro database e quello di manutenzione: nessun dato di terzi.
        pg_databases_list: vec!["odoo".to_string(), "postgres".to_string()],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(p)
            if p.ends_with("data"))),
        "con --aggressive-rollback e nessun database di terzi il cluster va via: {:?}",
        ops_of(&log)
    );
}

/// **La protezione che conta.** Con il flag ma con database di *altri* nel
/// cluster, il PGDATA non si tocca.
///
/// `cluster_safe_to_purge` è già la domanda giusta per il pacchetto — «c'è
/// qualcosa oltre al nostro database?» — e la sua risposta negativa protegge qui
/// esattamente come protegge lì. Un PGDATA contiene **tutti** i database del
/// cluster, non solo il nostro: rimuoverlo per annullare la nostra installazione
/// sarebbe l'anti-drop violato su scala maggiore.
#[test]
fn a_cluster_hosting_other_databases_is_never_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_databases_list: vec![
            "odoo".to_string(),
            "postgres".to_string(),
            "fatturazione_cliente".to_string(),
        ],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "nel cluster c'è un database che non abbiamo creato noi: il PGDATA li \
         contiene TUTTI, e rimuoverlo distruggerebbe dati del cliente"
    );
}

/// Un cluster **preesistente** non si rimuove nemmeno con il flag: non è nostro.
#[test]
fn a_preexisting_cluster_is_never_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_cluster_initialized: true,
        pg_databases_list: vec!["postgres".to_string()],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "PreState::Preexisting: c'era prima di noi, non è nostro da distruggere"
    );
}

// --- Retrocompatibilità dello snapshot --------------------------------------

/// Uno snapshot scritto **prima di M3** non ha il campo, e si legge come
/// `Untracked`.
///
/// È la verità per ogni installazione esistente: sono tutte Debian, dove il
/// cluster non lo abbiamo mai inizializzato noi. Renderlo illeggibile
/// significherebbe rendere non annullabile lo step — stessa cura di
/// `default_site` in R11 e di `InstallConfig` in R4.
#[test]
fn a_snapshot_written_before_this_axis_still_rehydrates() {
    let legacy = serde_json::json!({
        "installed": "CreatedByUs",
        "enabled": "CreatedByUs",
        "active": "CreatedByUs"
    });

    let (ops, _log) = MockSystemOps::new(MockConfig::default());
    let mut step = SetupPostgres::with_ops(Box::new(ops));
    step.rehydrate(&legacy)
        .expect("uno snapshot pre-M3 deve restare leggibile");

    let s = snap(&step);
    assert_eq!(
        s.installed,
        PreState::CreatedByUs,
        "gli assi noti si leggono"
    );
    assert_eq!(
        s.cluster_initialized,
        PreState::Untracked,
        "senza il campo, nessun cluster è nostro: è ciò che quelle installazioni erano"
    );
}

/// E il quarto asse sopravvive all'andata e ritorno, come gli altri tre.
#[test]
fn the_fourth_axis_survives_serialisation() {
    let (step, _log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    let serializzato = step.snapshot_value();

    let (ops, _log2) = MockSystemOps::new(MockConfig::default());
    let mut riletto = SetupPostgres::with_ops(Box::new(ops));
    riletto.rehydrate(&serializzato).expect("rehydrate");

    assert_eq!(snap(&riletto).cluster_initialized, PreState::CreatedByUs);
}

// --- Le implementazioni reali, non quelle del mock --------------------------

/// **Il buco che la validazione per mutazione ha trovato.**
///
/// I test sopra passano dal mock, che ha una *sua* `postgres_data_dir` scelta in
/// base alla famiglia modellata. Va bene per verificare la logica dello step —
/// ma significa che le implementazioni vere di `Debian` e `Fedora` non erano
/// esercitate da niente: la mutazione «Debian dichiara un PGDATA da
/// inizializzare» sopravviveva a tutta la suite.
///
/// In campo l'effetto sarebbe stato un `postgresql-setup --initdb` su Ubuntu,
/// dove quel comando non esiste — e uno step che fallisce dopo aver installato
/// PostgreSQL, cioè un rollback per una ragione inventata.
///
/// È la stessa lezione di R9 in un'altra forma: quando il mock replica una
/// decisione della produzione, la decisione va provata **anche** dov'è scritta.
#[test]
fn each_family_declares_its_own_cluster_policy() {
    use invok::distro::{debian::Debian, fedora::Fedora, Distro};

    assert_eq!(
        Debian::new().postgres_data_dir(),
        None,
        "su Debian/Ubuntu il postinst del pacchetto crea e avvia il cluster: \
         dichiarare un PGDATA qui farebbe eseguire un initdb che non serve, con \
         un comando che su quella famiglia non esiste"
    );

    assert_eq!(
        Fedora::new().postgres_data_dir(),
        Some(std::path::PathBuf::from("/var/lib/pgsql/data")),
        "su Fedora il cluster va creato, e il percorso è quello di default della \
         distribuzione"
    );
}

/// L'init su una famiglia che non ne ha bisogno è un **no-op che riesce**, non
/// un errore.
///
/// Conta perché lo step lo chiama solo quando `postgres_data_dir` è `Some`: se
/// qui ci fosse un `unreachable!()` o un errore, sarebbe un ramo che non può
/// eseguire — e il giorno che qualcuno cambiasse quella condizione, il difetto
/// arriverebbe in campo invece che qui.
#[test]
fn initialising_where_it_is_not_needed_succeeds_quietly() {
    use invok::distro::{debian::Debian, Distro};

    assert!(
        Debian::new().init_postgres_cluster().is_ok(),
        "su questa famiglia non c'è nulla da fare, e «nulla da fare» è un successo"
    );
}

// --- A-MD-6: le due risposte su «dov'è il cluster» ---------------------------

/// `systemctl show -p Environment postgresql.service`, letto come lo legge
/// `postgresql-setup`.
///
/// Il fixture è il formato **vero** del comando: una riga `Environment=` con più
/// assegnazioni separate da spazi. Scriverlo a memoria come una riga per
/// variabile darebbe un parser che non combacia con niente — è già successo due
/// volte in questo progetto, e sempre così (A-R8-1-ter).
#[test]
fn the_declared_pgdata_is_read_the_way_postgresql_setup_reads_it() {
    use invok::distro::fedora::pgdata_from_environment;
    use std::path::PathBuf;

    assert_eq!(
        pgdata_from_environment("Environment=PGDATA=/var/lib/pgsql/data\n"),
        Some(PathBuf::from("/var/lib/pgsql/data"))
    );
    assert_eq!(
        pgdata_from_environment(
            "Environment=PG_OOM_ADJUST_FILE=/proc/self/oom_score_adj PGDATA=/srv/pg/data\n"
        ),
        Some(PathBuf::from("/srv/pg/data")),
        "PGDATA non è per forza la prima variabile della riga"
    );
    assert_eq!(
        pgdata_from_environment("Environment=PGDATA=/var/lib/pgsql/data PGDATA=/srv/pg/data\n"),
        Some(PathBuf::from("/srv/pg/data")),
        "come in systemd, l'ultima definizione vince: è così che agisce un drop-in"
    );
    assert_eq!(
        pgdata_from_environment("Environment=\n"),
        None,
        "una unit senza PGDATA è «non lo so», non un percorso vuoto"
    );
    assert_eq!(
        pgdata_from_environment(""),
        None,
        "e nemmeno un output assente si traduce in un percorso"
    );
}

/// La regola: si rifiuta **solo** quando le due risposte divergono davvero.
#[test]
fn a_conflict_is_only_a_conflict_when_both_answers_exist_and_differ() {
    use invok::steps::setup_postgres::cluster_path_conflict;
    use std::path::Path;

    let nostro = Path::new("/var/lib/pgsql/data");

    assert!(
        cluster_path_conflict(nostro, Some(Path::new("/var/lib/pgsql/data"))).is_none(),
        "stesso percorso: è il caso normale di ogni Fedora"
    );
    assert!(
        cluster_path_conflict(nostro, None).is_none(),
        "«non lo so» non è un conflitto: cecità non è divergenza"
    );

    let messaggio = cluster_path_conflict(nostro, Some(Path::new("/srv/pg/data")))
        .expect("percorsi diversi: va rifiutato");
    assert!(
        messaggio.contains("/srv/pg/data") && messaggio.contains("/var/lib/pgsql/data"),
        "il messaggio deve nominare ENTRAMBI i percorsi, o non si capisce cosa non torna: \
         {messaggio}"
    );
    assert!(
        messaggio.contains("aggressive-rollback"),
        "e deve dire qual è il danno che si sta evitando: {messaggio}"
    );
}

/// Lo step si ferma **nello snapshot**, cioè prima di qualunque mutazione.
///
/// È la differenza fra una precondizione e un undo: qui non c'è niente da
/// annullare perché non è stato fatto niente. E il caso è raggiungibile davvero
/// — un PostgreSQL già installato con un drop-in che sposta PGDATA — che è
/// esattamente la macchina su cui il danno sarebbe peggiore, perché in
/// `/var/lib/pgsql/data` può esserci un cluster precedente del cliente.
#[test]
fn a_cluster_configured_elsewhere_stops_the_installation_before_it_starts() {
    let cfg = MockConfig {
        family: OsFamily::Fedora,
        pg_declared_data_dir: Some(std::path::PathBuf::from("/srv/pg/data")),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupPostgres::with_ops(Box::new(mock));

    let err = step
        .snapshot(&ctx(false))
        .expect_err("un PGDATA diverso dal nostro deve fermare lo step");
    assert!(
        matches!(err, StepError::Precondition(_)),
        "dev'essere una precondizione, non un errore qualsiasi: {err:?}"
    );
    assert!(
        ops_of(&log).is_empty(),
        "lo snapshot non deve aver mutato niente: {:?}",
        ops_of(&log)
    );
}

/// E quando le due risposte coincidono — o la unit non dice niente — lo step
/// procede come ha sempre fatto.
///
/// È la metà che rende il controllo un controllo: senza, «rifiuta sempre»
/// passerebbe il test qui sopra e nessuno se ne accorgerebbe.
#[test]
fn the_usual_fedora_is_not_stopped_by_this_check() {
    for dichiarato in [
        None,
        Some(std::path::PathBuf::from(
            invok::distro::fedora::POSTGRES_DATA_DIR,
        )),
    ] {
        let cfg = MockConfig {
            family: OsFamily::Fedora,
            pg_declared_data_dir: dichiarato.clone(),
            ..Default::default()
        };
        let (mock, _log) = MockSystemOps::new(cfg);
        let mut step = SetupPostgres::with_ops(Box::new(mock));
        assert!(
            step.snapshot(&ctx(false)).is_ok(),
            "con PGDATA {dichiarato:?} lo step deve procedere"
        );
    }
}
