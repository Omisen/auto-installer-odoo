//! Test del pattern delta (Fase 4): l'undo purga solo il delta, mai i preesistenti.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::{InstallState, StepRecord};
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::{
    AptDeltaSnapshot, AptPackagesStep, PackageSpec, UndoPolicy, ODOO_DEPENDENCIES,
};
use odoo_installer::system_ops::has_installable_candidate;

fn ctx(aggressive: bool) -> Context {
    Context {
        dry_run: false,
        aggressive_rollback: aggressive,
        ..Default::default()
    }
}

fn installed(pkgs: &[&str]) -> HashSet<String> {
    pkgs.iter().map(|s| s.to_string()).collect()
}

fn snapshot_of(step: &AptPackagesStep) -> AptDeltaSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn undo_purges_only_the_delta() {
    // 'a' già installato → delta = [b, c]. L'undo deve purgare solo [b, c].
    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.already_installed, strings(&["a"]));
    assert_eq!(snap.delta, strings(&["b", "c"]));

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // Purge del solo delta, mai 'a'.
    assert!(
        ops.contains(&Op::AptPurge(strings(&["b", "c"]))),
        "atteso purge di [b,c], trovato: {ops:?}"
    );
    // A3.3/A3.2: nessun `apt-get autoremove` globale nel rollback — rimuoverebbe
    // orfani estranei a Odoo, fuori dal nostro delta.
    assert!(
        !ops.contains(&Op::AptAutoremove),
        "l'undo non deve lanciare un autoremove globale, trovato: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::AptPurge(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non deve mai essere purgato"
    );
}

// --- A-RT-2: il purge deve funzionare anche con dpkg rotto ------------------
//
// Trovato in campo (Multipass, Ubuntu 22.04): il rollback arriva qui dopo che
// uno step a valle ha lasciato dpkg in stato inconsistente, apt si rifiuta di
// operare, il purge fallisce e i 24 pacchetti del delta restano installati.

#[test]
fn undo_recovers_a_broken_dpkg_before_purging() {
    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        dpkg_broken: true, // uno step a valle ha rotto dpkg
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo best-effort");

    let ops = ops_of(&log);
    let fix = ops
        .iter()
        .position(|o| matches!(o, Op::AptFixBroken))
        .expect("il recovery di dpkg deve precedere il purge");
    let purge = ops
        .iter()
        .position(|o| matches!(o, Op::AptPurge(_)))
        .expect("il purge deve essere tentato");
    assert!(
        fix < purge,
        "fix-broken prima del purge, altrimenti apt rifiuta: {ops:?}"
    );
    // E il purge, dopo il recovery, riesce davvero: il delta se ne va.
    assert!(
        ops.contains(&Op::AptPurge(strings(&["b", "c"]))),
        "il delta deve essere purgato dopo il recovery: {ops:?}"
    );
    // La protezione resta: mai il preesistente.
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::AptPurge(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non va mai purgato"
    );
}

#[test]
fn undo_retries_the_purge_after_dpkg_configure_all() {
    // `apt-get install -f` non basta (fallisce a sua volta): si passa a
    // `dpkg --configure -a` e si ritenta il purge.
    let cfg = MockConfig {
        dpkg_broken: true,
        fix_broken_fails: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo best-effort");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::DpkgConfigureAll),
        "se il fix-broken fallisce si tenta dpkg --configure -a: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::AptPurge(_))).count(),
        2,
        "il purge va ritentato dopo il recovery: {ops:?}"
    );
}

#[test]
fn undo_stays_best_effort_when_recovery_fails_completely() {
    // Nessun recovery funziona: l'undo **non deve fallire** (invariante 3), i
    // pacchetti restano e l'utente lo scopre dai log.
    let cfg = MockConfig {
        dpkg_broken: true,
        fix_broken_fails: true,
        dpkg_configure_fails: true,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c)
        .expect("undo best-effort: non propaga mai l'errore");
}

#[test]
fn empty_delta_is_noop() {
    // Tutti già installati → niente install, niente purge.
    let cfg = MockConfig {
        installed_packages: installed(&["a", "b", "c"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-empty",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert!(snapshot_of(&step).delta.is_empty());
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "delta vuoto → nessuna azione, trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn bootstrap_does_not_purge_on_normal_undo() {
    // Bootstrap: undo normale non purga git/curl/wget/gettext.
    let cfg = MockConfig::default(); // niente già installato → delta = tutti
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false); // rollback NON aggressivo

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AptInstall(_))),
        "il run deve installare"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::AptPurge(_))),
        "undo normale non deve purgare le utility bootstrap, trovato: {ops:?}"
    );
}

#[test]
fn bootstrap_purges_only_with_aggressive_rollback() {
    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(true); // --aggressive-rollback

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AptPurge(_))),
        "con --aggressive-rollback il bootstrap deve purgare il delta"
    );
    // Nemmeno in modalità aggressiva: l'autoremove globale è fuori dal delta.
    assert!(
        !ops.contains(&Op::AptAutoremove),
        "nessun autoremove globale nemmeno con --aggressive-rollback, trovato: {ops:?}"
    );
}

#[test]
fn deps_delta_excludes_bootstrap_overlap() {
    // 'git' già installato (dal bootstrap) → non finisce nel delta dei deps.
    let cfg = MockConfig {
        installed_packages: installed(&["git"]),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);

    assert!(snap.already_installed.contains(&"git".to_string()));
    assert!(
        !snap.delta.contains(&"git".to_string()),
        "git non deve essere nel delta"
    );
    assert!(
        snap.delta.contains(&"python3-pip".to_string()),
        "gli altri deps restano nel delta"
    );
}

// --- A5.1: nomi di pacchetto che cambiano tra release OS --------------------
//
// Confermato in campo dal job `container` di R5: su Debian 12 `libjpeg8-dev` non
// ha candidato e `libtiff5-dev` è diventato `libtiff-dev`, quindi
// `install-system-dependencies` falliva sull'intero gruppo e l'installazione non
// partiva. Qui il gruppo di alternative va provato in tutte le sue diramazioni.

/// Uno step con un solo gruppo di alternative, per isolare la risoluzione.
fn step_with_group(mock: MockSystemOps, group: &[&str]) -> AptPackagesStep {
    AptPackagesStep::with_specs(
        Box::new(mock),
        "test-alternatives",
        vec![PackageSpec::any(group)],
        UndoPolicy::PurgeDelta,
    )
}

#[test]
fn the_preferred_name_wins_when_available() {
    // Ubuntu 22.04: `libtiff5-dev` esiste → si usa quello, non il fallback.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(snapshot_of(&step).delta, strings(&["libtiff5-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::AptInstall(strings(&["libtiff5-dev"]))),
        "apt deve ricevere il nome preferito: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_fallback_is_used_when_the_preferred_name_does_not_exist() {
    // Debian 12: `libtiff5-dev` non ha candidato → si installa `libtiff-dev`, e
    // lo step NON fallisce (era il bug: l'installer non partiva affatto).
    let cfg = MockConfig {
        packages_without_candidate: installed(&["libtiff5-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c)
        .expect("snapshot: il fallback deve salvare lo step");
    assert_eq!(snapshot_of(&step).delta, strings(&["libtiff-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::AptInstall(strings(&["libtiff-dev"]))),
        "apt deve ricevere il fallback, mai il nome inesistente: {:?}",
        ops_of(&log)
    );

    // E il rollback purga il nome davvero installato.
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::AptPurge(strings(&["libtiff-dev"]))),
        "il delta persistito è in nomi risolti: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_already_installed_alternative_wins_over_the_preferred_one() {
    // Il cliente ha `libtiff-dev`. Installare anche `libtiff5-dev` sarebbe
    // corretto ma inutile — e finirebbe nel delta, cioè in qualcosa che il
    // rollback poi purga da un sistema che non ce l'aveva chiesto.
    let cfg = MockConfig {
        installed_packages: installed(&["libtiff-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.already_installed, strings(&["libtiff-dev"]));
    assert!(
        snap.delta.is_empty(),
        "un'alternativa già presente non è nostra: delta vuoto, trovato {:?}",
        snap.delta
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::AptPurge(_))),
        "niente da purgare: non abbiamo installato nulla. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_group_with_no_installable_alternative_fails_before_mutating() {
    // Nessun nome disponibile: lo step si ferma nello *snapshot*, prima di
    // toccare apt. Degradare in silenzio sposterebbe l'errore dentro un
    // `pip install` che non compila, molto più difficile da diagnosticare.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["sparito-dev", "sparito2-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::with_specs(
        Box::new(mock),
        "test-alternatives",
        vec![
            PackageSpec::one("presente-dev"),
            PackageSpec::any(&["sparito-dev", "sparito2-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );

    let err = step
        .snapshot(&ctx(false))
        .expect_err("un gruppo vuoto deve essere un errore");
    let message = err.to_string();
    assert!(
        message.contains("sparito-dev") && message.contains("sparito2-dev"),
        "il messaggio deve dire quale gruppo è vuoto: {message}"
    );
    assert!(
        message.contains("A5.1"),
        "e come chiuderlo (aggiungere l'alternativa): {message}"
    );
    assert!(
        ops_of(&log).is_empty(),
        "nessuna mutazione prima dell'errore: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_empty_apt_cache_is_diagnosed_as_such() {
    // Caso ambiguo che vale la pena distinguere: se NESSUN nome della lista è
    // disponibile, il problema non sono le rinomine ma le liste apt vuote (un
    // container appena creato). Mandare l'utente a cercare pacchetti rinominati
    // sarebbe una diagnosi sbagliata.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["a", "b"]),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-cache-vuota",
        strings(&["a", "b"]),
        UndoPolicy::PurgeDelta,
    );

    let message = step
        .snapshot(&ctx(false))
        .expect_err("nessun pacchetto disponibile → errore")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "con la cache apt vuota il messaggio deve indicare 'apt-get update': {message}"
    );
}

#[test]
fn an_optional_group_without_candidates_does_not_stop_the_install() {
    // `node-less` è stato rimosso da alcune release Debian ed è un "nice to
    // have" (Odoo compila SCSS in-process). Un opzionale mancante è un warning,
    // non un'installazione impossibile — ma tutto il resto viene installato.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["node-less"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::with_specs(
        Box::new(mock),
        "test-opzionale",
        vec![
            PackageSpec::one("build-essential"),
            PackageSpec::optional(&["node-less"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c)
        .expect("un opzionale mancante non è un errore");
    assert_eq!(snapshot_of(&step).delta, strings(&["build-essential"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::AptInstall(strings(&["build-essential"]))),
        "il pacchetto disponibile va installato comunque: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_canonical_list_declares_the_renames_seen_in_the_field() {
    // Guardia sulla lista di produzione: i due nomi che hanno rotto Debian 12 in
    // R5 devono avere un'alternativa. Senza questo test, una futura pulizia
    // della lista potrebbe riportarli a nomi secchi e riaprire A5.1 in silenzio.
    let groups: Vec<Vec<&str>> = ODOO_DEPENDENCIES.iter().map(|g| g.to_vec()).collect();
    for broken in ["libtiff5-dev", "libjpeg8-dev"] {
        let group = groups
            .iter()
            .find(|g| g.contains(&broken))
            .unwrap_or_else(|| panic!("'{broken}' deve restare in lista"));
        assert!(
            group.len() > 1,
            "'{broken}' non esiste su Debian 12: il suo gruppo deve avere un'alternativa, \
             trovato {group:?}"
        );
    }
}

#[test]
fn apt_cache_policy_output_is_parsed_as_apt_prints_it() {
    // Il parser gira su output reale di `apt-cache policy` (catturato su Ubuntu
    // 24.04). I tre casi che contano sono indistinguibili se si guarda solo
    // l'exit code: disponibile, virtuale senza candidato, nome inesistente.
    let available =
        "libtiff-dev:\n  Installed: (none)\n  Candidate: 4.5.1+git230720-4ubuntu2.5\n  \
                     Version table:\n     4.5.1+git230720-4ubuntu2.5 500\n";
    let virtual_only = "libjpeg8-dev:\n  Installed: (none)\n  Candidate: (none)\n  \
                        Version table:\n";
    let missing = "N: Unable to locate package libtiff5-dev\n";

    assert!(has_installable_candidate(available));
    assert!(
        !has_installable_candidate(virtual_only),
        "'Candidate: (none)' non è installabile"
    );
    assert!(
        !has_installable_candidate(missing),
        "senza riga Candidate il pacchetto non esiste"
    );
    assert!(!has_installable_candidate(""));
}

#[test]
fn delta_survives_state_save_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "persisted",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&Context::default()).expect("snapshot");

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: step.name().to_string(),
        snapshot: step.snapshot_value(),
    });
    state.save(&path).expect("save");

    let reloaded = InstallState::load(&path).expect("load");
    let snap: AptDeltaSnapshot =
        serde_json::from_value(reloaded.completed[0].snapshot.clone()).expect("delta");
    assert_eq!(snap.delta, strings(&["b", "c"]));
    assert_eq!(snap.already_installed, strings(&["a"]));
}
