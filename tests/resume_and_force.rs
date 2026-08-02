//! A-V3-1: un manifesto già sul disco non viene mai sovrascritto in silenzio.
//!
//! Due livelli, entrambi senza root e senza toccare il sistema:
//! - la **politica** (`state::start_decision`) — installare, riprendere o
//!   rifiutare — che è una funzione pura;
//! - il **resume del motore**, cioè cosa succede davvero agli step già eseguiti.
//!
//! Il difetto che questi test presidiano non era in uno step: era in `main`,
//! fra pezzi coperti singolarmente. Per questo la regola è stata spostata in
//! libreria: perché un test potesse raggiungerla.

use std::sync::{Arc, Mutex};

use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::state::{
    start_decision, InstallConfig, InstallState, PreState, StartDecision, StepRecord,
};
use odoo_installer::step::Step;
use odoo_installer::steps::noop::{NoopStep, UndoLog};

// --- helper ------------------------------------------------------------------

fn ctx_with_state(dir: &tempfile::TempDir) -> Context {
    Context {
        dry_run: false,
        odoo_version: "18.0".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"))
}

/// Manifesto di un'installazione interrotta: alcuni step registrati, non conclusa.
fn partial_state(ctx: &Context, steps: &[(&str, PreState)]) -> InstallState {
    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(ctx));
    for (name, prestate) in steps {
        state.record(StepRecord {
            name: (*name).to_string(),
            snapshot: serde_json::to_value(prestate).expect("serialize"),
        });
    }
    state
}

// --- la politica: installare, riprendere, rifiutare ---------------------------

#[test]
fn no_manifest_means_a_first_installation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let decision = start_decision(&InstallState::default(), &cfg, false);

    assert_eq!(decision, StartDecision::Fresh);
}

/// Il caso che dà il nome ad A-V3-1: una seconda installazione su un'istanza
/// completa. Prima veniva accettata, e il manifesto — l'unica traccia di cosa
/// rimuovere — veniva riscritto con tutto marcato come preesistente.
#[test]
fn a_finished_installation_is_refused_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    state.finished = true;

    assert_eq!(
        start_decision(&state, &cfg, false),
        StartDecision::RefuseFinished,
        "reinstallare sopra un'istanza completa deve fermarsi, non sovrascrivere il manifesto"
    );
}

/// `--force` è la via d'uscita esplicita: si reinstalla, ma il manifesto
/// precedente va messo da parte (l'archiviazione è di `main`, qui si verifica
/// che la politica la richieda).
#[test]
fn force_turns_a_refusal_into_a_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut finita = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    finita.finished = true;
    assert_eq!(start_decision(&finita, &cfg, true), StartDecision::Replace);

    let parziale = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    assert_eq!(
        start_decision(&parziale, &cfg, true),
        StartDecision::Replace
    );
}

/// Un manifesto parziale con gli stessi parametri è un'installazione
/// interrotta: si riprende. È il flusso che `CLAUDE.md` dichiara supportato
/// («rilancia e prosegui»), e quello che prima perdeva la proprietà degli step
/// già eseguiti.
#[test]
fn a_partial_manifest_with_the_same_parameters_is_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    assert_eq!(start_decision(&state, &cfg, false), StartDecision::Resume);
}

/// Riprendere con parametri diversi produrrebbe un manifesto a metà fra due
/// istanze: metà undo punterebbero altrove. Nel caso del database è la
/// violazione diretta dell'anti-drop.
#[test]
fn resuming_with_different_artifacts_is_refused_and_says_which() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut altra = ctx_with_state(&dir);
    altra.db_name = "fatturazione".to_string();
    let cfg = InstallConfig::from_context(&altra);

    let StartDecision::RefuseIdentityMismatch(differenze) = start_decision(&state, &cfg, false)
    else {
        panic!("atteso un rifiuto per identità diversa");
    };

    assert_eq!(differenze.len(), 1, "una sola differenza: il database");
    assert_eq!(
        differenze[0],
        (
            "nome database",
            "citest".to_string(),
            "fatturazione".to_string()
        ),
        "il messaggio deve poter dire QUALE artefatto non coincide"
    );
}

/// I campi che non nominano un artefatto non bloccano il resume: cambiare porta
/// o amministratore che rilancia non rende il manifesto incoerente.
#[test]
fn non_identifying_fields_do_not_block_a_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut diverso = ctx_with_state(&dir);
    diverso.port = 8888;
    diverso.sudo_user = Some("un-altro-admin".to_string());
    let cfg = InstallConfig::from_context(&diverso);

    assert_eq!(start_decision(&state, &cfg, false), StartDecision::Resume);
}

/// Manifesto pre-R4, senza configurazione: non si può stabilire se descriva gli
/// stessi artefatti. Fail-closed, come per uno snapshot illeggibile.
#[test]
fn a_manifest_without_config_is_not_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    assert_eq!(
        start_decision(&state, &cfg, false),
        StartDecision::RefuseUnknownIdentity
    );
}

// --- il resume del motore -----------------------------------------------------

/// Uno step già registrato non viene rieseguito. Osservabile senza spiare il
/// motore: lo step live è configurato per **fallire** al `run`, quindi se
/// l'esecuzione arriva in fondo è perché non è stato rieseguito.
#[test]
fn resume_does_not_re_run_completed_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_run()),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::resuming_from(state);
    installer
        .execute(&mut steps, &ctx)
        .expect("alpha è già stato eseguito: non va rifatto");

    assert_eq!(
        installer.state().completed.len(),
        2,
        "il manifesto deve contenere alpha (ereditato) e beta (eseguito ora), una volta ciascuno"
    );
    let nomi: Vec<&str> = installer
        .state()
        .completed
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(nomi, vec!["alpha", "beta"]);
}

/// **Il test del difetto.** Al rilancio, uno step già eseguito vedrebbe il
/// proprio artefatto già presente e lo dichiarerebbe `Preexisting`: corretto
/// come fotografia, disastroso come verdetto di proprietà. Con il resume la
/// proprietà si **rilegge** dal manifesto, e l'undo continua ad agire.
#[test]
fn resume_inherits_ownership_instead_of_re_deducing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // Il manifesto dice: alpha l'abbiamo creato noi.
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // Lo step live è configurato come lo vedrebbe uno snapshot rifatto oggi:
    // l'artefatto c'è già, quindi "preesistente".
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(
            NoopStep::new("alpha")
                .preexisting()
                .with_undo_log(Arc::clone(&log)),
        ),
        Box::new(
            NoopStep::new("beta")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::resuming_from(state);
    let err = installer.execute(&mut steps, &ctx);
    assert!(err.is_err(), "beta fallisce e innesca il rollback");

    let azioni = log.lock().expect("log").clone();
    assert_eq!(
        azioni,
        vec!["alpha".to_string()],
        "l'undo di alpha deve AGIRE: il manifesto dice che l'artefatto è nostro. \
         Senza eredità sarebbe stato Preexisting e l'artefatto resterebbe sulla macchina \
         per sempre — è esattamente il danno di A-V3-1"
    );
}

/// La configurazione registrata non viene sovrascritta da quella corrente: è
/// l'identità degli artefatti su cui agiranno gli undo.
#[test]
fn resume_keeps_the_recorded_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let originale = InstallConfig::from_context(&ctx);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // Il motore riceve un Context con un database diverso: se sovrascrivesse la
    // config, gli undo del manifesto punterebbero al database sbagliato.
    let mut altro = ctx_with_state(&dir);
    altro.db_name = "fatturazione".to_string();

    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(NoopStep::new("alpha"))];
    let mut installer = Installer::resuming_from(state);
    installer.execute(&mut steps, &altro).expect("execute");

    assert_eq!(
        installer.state().config.as_ref(),
        Some(&originale),
        "la config del manifesto è l'identità degli artefatti: non si riscrive a metà corsa"
    );
}

/// Uno snapshot persistito illeggibile ferma il resume invece di inventare un
/// verdetto: stesso fail-closed del rollback da disco.
#[test]
fn an_unreadable_snapshot_stops_the_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(&ctx));
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::json!({"non": "un PreState"}),
    });

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::resuming_from(state);
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "senza uno snapshot leggibile non sappiamo di chi sia l'artefatto: meglio fermarsi"
    );
}

/// Un'installazione ripresa e portata a termine produce un manifesto **unico**:
/// gli step ereditati non vengono duplicati e il flag di conclusione c'è.
#[test]
fn a_resumed_installation_finishes_with_a_single_coherent_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(
        &ctx,
        &[
            ("alpha", PreState::CreatedByUs),
            ("beta", PreState::Preexisting),
        ],
    );

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_run()),
        Box::new(NoopStep::new("beta").fail_on_run()),
        Box::new(NoopStep::new("gamma")),
    ];

    let mut installer = Installer::resuming_from(state);
    installer.execute(&mut steps, &ctx).expect("execute");
    installer.mark_finished(&ctx).expect("mark_finished");

    let riletto = InstallState::load(&ctx.state_path).expect("load");
    let nomi: Vec<&str> = riletto.completed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(nomi, vec!["alpha", "beta", "gamma"]);
    assert!(riletto.finished, "l'installazione ripresa è conclusa");

    // Le proprietà ereditate sopravvivono al giro completo.
    let alpha: PreState =
        serde_json::from_value(riletto.completed[0].snapshot.clone()).expect("prestate");
    let beta: PreState =
        serde_json::from_value(riletto.completed[1].snapshot.clone()).expect("prestate");
    assert_eq!(alpha, PreState::CreatedByUs);
    assert_eq!(beta, PreState::Preexisting);
}

// --- A-R9-1: la porta occupata da noi stessi non blocca il resume ------------

/// Il preflight sulla porta esiste per intercettare un conflitto con **terzi**.
/// Dopo `setup-systemd` il servizio in ascolto è il nostro: un'installazione
/// interrotta allo step 18 di 24 non deve diventare irriprendibile per colpa del
/// servizio che ha appena installato.
#[test]
fn a_manifest_past_setup_systemd_owns_the_http_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let prima = partial_state(
        &ctx,
        &[
            ("prepare-opt-root", PreState::CreatedByUs),
            ("create-database", PreState::CreatedByUs),
        ],
    );
    assert!(
        !prima.owns_the_http_port(),
        "senza setup-systemd la porta non è nostra: un conflitto è reale e va segnalato"
    );

    let dopo = partial_state(
        &ctx,
        &[
            ("prepare-opt-root", PreState::CreatedByUs),
            ("setup-systemd", PreState::CreatedByUs),
        ],
    );
    assert!(
        dopo.owns_the_http_port(),
        "con setup-systemd registrato la porta è del nostro servizio: il controllo va saltato, \
         o il resume muore sul servizio che ha appena installato (A-R9-1)"
    );
}

/// Un manifesto vuoto non possiede nulla: una prima installazione deve passare
/// dal controllo sulla porta come sempre.
#[test]
fn an_empty_manifest_owns_no_port() {
    assert!(!InstallState::default().owns_the_http_port());
}
