//! A-V3-1: a manifest already on disk is never silently overwritten.
//!
//! two levels, both unprivileged: the **policy** — install, resume or refuse —
//! which is a pure function, and the **engine's resume**, i.e. what really
//! happens to steps that already ran.
//!
//! the defect these guard was not in a step but in `main`, between pieces each
//! covered on its own. that is why the rule moved into the library: so a test
//! could reach it.

use std::sync::{Arc, Mutex};

use invok::context::Context;
use invok::engine::Installer;
use invok::state::{
    start_decision, InstallConfig, InstallState, PreState, StartDecision, StepRecord,
};
use invok::step::Step;
use invok::steps::noop::{NoopStep, UndoLog};

// --- helpers ----------------------------------------------------------------

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

/// the manifest of an interrupted installation: some steps, not finished.
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

// --- the policy: install, resume, refuse ------------------------------------

#[test]
fn no_manifest_means_a_first_installation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let decision = start_decision(&InstallState::default(), &cfg, false);

    assert_eq!(decision, StartDecision::Fresh);
}

/// the case A-V3-1 is named for: a second installation over a complete one. it
/// used to be accepted, and the manifest — the only record of what to remove —
/// was rewritten with everything marked pre-existing.
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

/// `--force` is the explicit way out: reinstall, but set the previous manifest
/// aside. archiving is `main`'s job; this checks the policy demands it.
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

/// a partial manifest with the same parameters is an interrupted installation,
/// so it resumes. the supported flow, and the one that used to lose ownership
/// of the steps already run.
#[test]
fn a_partial_manifest_with_the_same_parameters_is_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    assert_eq!(start_decision(&state, &cfg, false), StartDecision::Resume);
}

/// resuming with different parameters would produce a manifest straddling two
/// instances, with half the undos pointing elsewhere. for the database that is
/// a direct anti-drop violation.
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
            "database name",
            "citest".to_string(),
            "fatturazione".to_string()
        ),
        "il messaggio deve poter dire QUALE artefatto non coincide"
    );
}

/// fields that name no artifact do not block a resume: a different port, or a
/// different administrator, does not make the manifest incoherent.
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

/// a pre-R4 manifest with no configuration: identity cannot be established, so
/// fail-closed, as with an unreadable snapshot.
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

// --- the engine's resume ----------------------------------------------------

/// an already-recorded step is not re-run. observable without spying on the
/// engine: the live step is configured to **fail** its run, so reaching the end
/// proves it was not re-run.
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

/// **the defect's test.** on a re-run, a step that already executed would see
/// its own artifact and declare it `Preexisting`: correct as a photograph,
/// disastrous as a verdict of ownership. with the resume, ownership is
/// **re-read** from the manifest and the undo keeps acting.
#[test]
fn resume_inherits_ownership_instead_of_re_deducing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // the manifest says we created it.
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // the live step is configured as a fresh snapshot would see it today: the
    // artifact is there, hence "pre-existing".
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

/// the recorded configuration is not overwritten by the current one: it is the
/// identity of the artifacts the undos will act on.
#[test]
fn resume_keeps_the_recorded_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let originale = InstallConfig::from_context(&ctx);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // the engine gets a context naming a different database: overwriting the
    // config would point the undos at the wrong one.
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

/// an unreadable persisted snapshot stops the resume rather than inventing a
/// verdict: the same fail-closed rule as the rollback from disk.
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

/// an installation resumed and completed produces a **single** manifest:
/// inherited steps are not duplicated, and the finished flag is set.
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

    // the inherited ownership survives the whole pass.
    let alpha: PreState =
        serde_json::from_value(riletto.completed[0].snapshot.clone()).expect("prestate");
    let beta: PreState =
        serde_json::from_value(riletto.completed[1].snapshot.clone()).expect("prestate");
    assert_eq!(alpha, PreState::CreatedByUs);
    assert_eq!(beta, PreState::Preexisting);
}

// --- A-R9-1: a port held by ourselves does not block the resume -------------

/// the port preflight exists to catch a conflict with **somebody else**. once
/// the service step has run, the listener is ours: an installation interrupted
/// after it must not become unresumable because of the service it just
/// installed.
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

/// an empty manifest owns nothing: a first installation still goes through the
/// port check.
#[test]
fn an_empty_manifest_owns_no_port() {
    assert!(!InstallState::default().owns_the_http_port());
}
