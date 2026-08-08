//! Test end-to-end del motore contro le 4 invarianti di `CLAUDE.md`.
//!
//! Non toccano il sistema: usano `NoopStep` e una directory temporanea per il
//! file di stato, quindi girano senza root.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use invok::context::Context;
use invok::engine::Installer;
use invok::state::{InstallState, PreState, StepRecord};
use invok::step::Step;
use invok::steps::noop::{NoopStep, UndoLog};

/// Context non-dry con file di stato in una tempdir (niente root, niente `/opt`).
/// Il motore usa solo `dry_run` e `state_path`; il resto è irrilevante qui.
fn ctx_with_state(dir: &tempfile::TempDir) -> Context {
    Context {
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join(".installer-state.json"))
}

/// Invariante 2: il rollback esegue gli `undo` dall'ultimo completato al primo.
#[test]
fn rollback_runs_undo_in_reverse_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // alpha, beta completano; gamma fallisce al run e innesca il rollback.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("beta").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("gamma")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // Solo alpha e beta erano completati; gamma non ha completato → niente undo.
    // Ordine inverso: prima beta, poi alpha.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
}

/// Invariante 3: un `undo` che fallisce non impedisce agli altri di eseguire.
#[test]
fn rollback_is_best_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // beta fallirà l'undo; alpha deve comunque essere ripulito dopo.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("beta")
                .fail_on_undo()
                .with_undo_log(Arc::clone(&log)),
        ),
        Box::new(
            NoopStep::new("gamma")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // beta agisce (e fallisce), ma alpha viene comunque ripulito dopo di lui.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
}

/// Invariante 3 / protezione: uno step `Preexisting` non compie azioni di undo,
/// pur essendo `undo` invocato dal motore.
#[test]
fn preexisting_step_undo_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // alpha è preesistente (non nostro): undo deve essere NO-OP.
    let alpha = NoopStep::new("alpha")
        .preexisting()
        .with_undo_log(Arc::clone(&log));
    let alpha_calls = alpha.undo_call_handle();

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(alpha),
        Box::new(
            NoopStep::new("beta")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su beta");
    // undo è stato invocato su alpha...
    assert_eq!(
        alpha_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "undo di alpha deve essere invocato dal motore"
    );
    // ...ma non ha compiuto alcuna azione (log vuoto: nessun artefatto nostro).
    assert!(
        log.lock().expect("lock").is_empty(),
        "un undo su Preexisting non deve compiere azioni"
    );
}

/// Invariante 4: lo stato scritto e riletto mantiene i `completed`; permessi 0600.
#[test]
fn install_state_roundtrip_and_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".installer-state.json");

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });
    state.record(StepRecord {
        name: "beta".to_string(),
        snapshot: serde_json::to_value(PreState::Preexisting).expect("serialize"),
    });

    state.save(&path).expect("save");

    // Permessi 0600.
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "il file di stato deve essere 0600");

    // Round-trip: i record sopravvivono al ciclo scrittura/lettura.
    let reloaded = InstallState::load(&path).expect("load");
    assert_eq!(reloaded, state);
    assert_eq!(reloaded.completed.len(), 2);
    assert_eq!(reloaded.completed[0].name, "alpha");
    assert_eq!(reloaded.completed[1].name, "beta");

    // load su file assente → stato vuoto (prima esecuzione), non errore.
    InstallState::clear(&path).expect("clear");
    let empty = InstallState::load(&path).expect("load assente");
    assert!(empty.completed.is_empty());
}

/// Percorso felice: tutti gli step completano, lo stato è persistito su disco.
#[test]
fn successful_run_persists_all_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::new();
    installer.execute(&mut steps, &ctx).expect("esecuzione ok");

    assert_eq!(installer.state().completed.len(), 2);

    let persisted = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(persisted.completed.len(), 2);
    assert_eq!(persisted.completed[0].name, "alpha");
    assert_eq!(persisted.completed[1].name, "beta");
}

// --- B-V3-5: interruzione dall'esterno --------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

/// **Il difetto che chiude.** Un Ctrl-C uccideva il processo all'istante, quindi
/// il rollback in-process non partiva mai e il sistema restava a metà. Ora
/// l'interruzione è una richiesta che il motore osserva: gli step già eseguiti
/// vengono annullati, in ordine inverso, come per un fallimento.
#[test]
fn an_interrupt_rolls_back_what_was_already_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));
    let interrupted = Arc::new(AtomicBool::new(false));

    // `beta` alza il flag mentre gira: è ciò che fa un Ctrl-C durante uno step.
    let flag_per_beta = Arc::clone(&interrupted);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("beta")
                .with_undo_log(Arc::clone(&log))
                .on_run(move || flag_per_beta.store(true, Ordering::SeqCst)),
        ),
        Box::new(NoopStep::new("gamma").with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new().watching_interrupt(Arc::clone(&interrupted));
    let err = installer
        .execute(&mut steps, &ctx)
        .expect_err("un'interruzione deve fermare l'esecuzione");
    assert!(
        err.to_string().contains("interrotta"),
        "il messaggio deve dire cosa è successo: {err}"
    );

    // `gamma` non è mai partito, e i due precedenti sono stati annullati in
    // ordine inverso — esattamente come per un fallimento.
    let azioni = log.lock().expect("log").clone();
    assert_eq!(
        azioni,
        vec!["beta".to_string(), "alpha".to_string()],
        "gli step già eseguiti vanno annullati dall'ultimo al primo: {azioni:?}"
    );
}

/// Lo step in corso viene **portato a termine**: fermarlo a metà lascerebbe
/// `dpkg` inconsistente o un database inizializzato a metà. Il confine sicuro è
/// quello che il motore già conosce — uno step o è completo o non è iniziato.
#[test]
fn the_step_in_flight_is_allowed_to_finish() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let interrupted = Arc::new(AtomicBool::new(false));
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    let flag = Arc::clone(&interrupted);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(
            NoopStep::new("alpha")
                .with_undo_log(Arc::clone(&log))
                .on_run(move || flag.store(true, Ordering::SeqCst)),
        ),
        Box::new(NoopStep::new("beta").with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new().watching_interrupt(Arc::clone(&interrupted));
    let _ = installer.execute(&mut steps, &ctx);

    // La prova che alpha è stato portato a termine è che il rollback lo
    // **annulla**: un undo viene invocato solo su uno step completato. Non la
    // si cerca nel manifesto, perché lì — giustamente — dopo il rollback alpha
    // non c'è più: il manifesto dice cosa resta, non cosa è stato fatto.
    let azioni = log.lock().expect("log").clone();
    assert_eq!(
        azioni,
        vec!["alpha".to_string()],
        "lo step in corso va completato (e quindi annullato); beta non è mai partito"
    );
    assert!(
        installer.state().completed.is_empty(),
        "annullato alpha, sul sistema non resta nulla: il manifesto deve dirlo"
    );
}

/// Senza interruzione il comportamento è quello di sempre: il flag di default
/// non è alzato da nessuno, e `watching_interrupt` è opzionale.
#[test]
fn without_an_interrupt_nothing_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    // Senza `watching_interrupt`.
    Installer::new()
        .execute(&mut steps, &ctx)
        .expect("nessuna interruzione: l'esecuzione arriva in fondo");

    // E con un flag mai alzato.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];
    Installer::new()
        .watching_interrupt(Arc::new(AtomicBool::new(false)))
        .execute(&mut steps, &ctx)
        .expect("flag mai alzato: nessun effetto");
}

/// **A-R8-1.** Dopo un fallimento, il rollback automatico annulla gli step già
/// eseguiti — e il manifesto **non deve continuare a elencarli**.
///
/// È il flusso che README e wiki descrivono da sempre: «se fallisce, correggi la
/// causa e rilancia». Con il resume introdotto in R8, un manifesto che elencava
/// step già annullati faceva saltare al rilancio proprio quelli: l'installazione
/// proseguiva dando per esistenti `/opt/odoo`, l'utente e il database che il
/// rollback aveva appena rimosso.
///
/// La regola che lo chiude: **il manifesto dice cosa c'è ancora sul sistema**,
/// non cosa è stato fatto a un certo punto.
#[test]
fn a_rolled_back_step_is_re_executed_on_the_next_run() {
    use std::sync::atomic::AtomicUsize;

    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    // Giro 1: alpha riesce, beta fallisce → il rollback annulla alpha.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    let dopo_rollback = InstallState::load(&ctx.state_path).expect("load");
    assert!(
        dopo_rollback.completed.is_empty(),
        "alpha è stato annullato: il manifesto non deve più elencarlo, trovato {:?}",
        dopo_rollback
            .completed
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
    );

    // Giro 2: si rilancia. alpha dev'essere RIESEGUITO, non saltato.
    let esecuzioni = Arc::new(AtomicUsize::new(0));
    let contatore = Arc::clone(&esecuzioni);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").on_run(move || {
            contatore.fetch_add(1, Ordering::SeqCst);
        })),
        Box::new(NoopStep::new("beta")),
    ];
    Installer::resuming_from(dopo_rollback)
        .execute(&mut steps, &ctx)
        .expect("il secondo giro deve arrivare in fondo");

    assert_eq!(
        esecuzioni.load(Ordering::SeqCst),
        1,
        "alpha era stato annullato: saltarlo lascerebbe l'installazione a costruire \
         su artefatti che non esistono"
    );
}

/// Ma un undo **fallito** lascia il record: lì l'artefatto è (forse) ancora sul
/// sistema, e quel record è l'unica traccia del residuo che
/// `invok rollback` potrà ritentare. Dimenticarlo sarebbe perdere
/// l'informazione proprio nel caso in cui serve.
#[test]
fn a_failed_undo_keeps_its_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_undo()),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    let stato = InstallState::load(&ctx.state_path).expect("load");
    let nomi: Vec<&str> = stato.completed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        nomi,
        vec!["alpha"],
        "l'undo di alpha è fallito: il residuo resta registrato"
    );
}

/// Se dopo il rollback non resta **niente**, il manifesto non deve restare
/// nemmeno lui.
///
/// Un file che descrive zero artefatti è un residuo che dice il falso: farebbe
/// credere a `invok rollback` che ci sia qualcosa da consumare, e
/// resterebbe sul disco a tempo indeterminato. Trovato dalla CI, che asseriva
/// — giustamente — che il manifesto fosse stato consumato.
#[test]
fn an_empty_manifest_is_removed_not_left_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    assert!(
        !ctx.state_path.exists(),
        "annullato tutto, il manifesto non descrive più niente: va rimosso, non svuotato"
    );
}

/// Ma se qualcosa resta — un undo fallito — il manifesto **deve** restare: è
/// l'unica traccia del residuo che `invok rollback` potrà ritentare.
#[test]
fn a_manifest_with_residue_stays_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_undo()),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    assert!(
        ctx.state_path.exists(),
        "c'è un residuo: il manifesto resta"
    );
    let stato = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(
        stato
            .completed
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
}
