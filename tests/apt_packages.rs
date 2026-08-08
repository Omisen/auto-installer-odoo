//! Test del pattern delta (Fase 4): l'undo purga solo il delta, mai i preesistenti.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::packaging::apt::AptBackend;
use invok::packaging::{PackageManager, PackageSpec};
use invok::state::{InstallState, StepRecord};
use invok::step::Step;
use invok::steps::apt_packages::{AptDeltaSnapshot, AptPackagesStep, UndoPolicy};
use invok::system_ops::{has_installable_candidate, total_package_names};

use common::model::{ModelState, SystemModel};

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
        ops.contains(&Op::PkgRemove(strings(&["b", "c"]))),
        "atteso purge di [b,c], trovato: {ops:?}"
    );
    // A3.3/A3.2: nessun `apt-get autoremove` globale nel rollback — rimuoverebbe
    // orfani estranei a Odoo, fuori dal nostro delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
        "l'undo non deve lanciare un autoremove globale, trovato: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
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
        .position(|o| matches!(o, Op::PkgRepair))
        .expect("il recovery di dpkg deve precedere il purge");
    let purge = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRemove(_)))
        .expect("il purge deve essere tentato");
    assert!(
        fix < purge,
        "fix-broken prima del purge, altrimenti apt rifiuta: {ops:?}"
    );
    // E il purge, dopo il recovery, riesce davvero: il delta se ne va.
    assert!(
        ops.contains(&Op::PkgRemove(strings(&["b", "c"]))),
        "il delta deve essere purgato dopo il recovery: {ops:?}"
    );
    // La protezione resta: mai il preesistente.
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
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
        ops.contains(&Op::PkgDeepRepair),
        "se il fix-broken fallisce si tenta dpkg --configure -a: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::PkgRemove(_))).count(),
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
        ops.iter().any(|op| matches!(op, Op::PkgInstall(_))),
        "il run deve installare"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::PkgRemove(_))),
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
        ops.iter().any(|op| matches!(op, Op::PkgRemove(_))),
        "con --aggressive-rollback il bootstrap deve purgare il delta"
    );
    // Nemmeno in modalità aggressiva: l'autoremove globale è fuori dal delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
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
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libtiff5-dev"]))),
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
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libtiff-dev"]))),
        "apt deve ricevere il fallback, mai il nome inesistente: {:?}",
        ops_of(&log)
    );

    // E il rollback purga il nome davvero installato.
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libtiff-dev"]))),
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
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgRemove(_))),
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
fn an_unusable_apt_index_is_diagnosed_as_such_not_as_a_missing_package() {
    // A5.1-bis, il cuore della diagnosi. Due condizioni si presentano identiche —
    // "nessun nome ha un candidato" — ma hanno cause opposte: il pacchetto non
    // esiste, oppure non possiamo *vedere* se esiste. Con l'indice inservibile la
    // seconda è l'unica conclusione lecita, e il messaggio deve dirlo: in campo
    // quello sbagliato ha mandato a cercare la rinomina di `libfreetype6-dev`,
    // che era al suo posto.
    let cfg = MockConfig {
        apt_index_populated: false, // apt-get update mai eseguito
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-indice-inservibile",
        strings(&["libfreetype6-dev", "libxml2-dev"]),
        UndoPolicy::PurgeDelta,
    );

    let message = step
        .snapshot(&ctx(false))
        .expect_err("indice inservibile → errore, ma con la diagnosi giusta")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "con l'indice inservibile il messaggio deve indicare 'apt-get update': {message}"
    );
    assert!(
        !message.contains("non esistono su questa release"),
        "e NON deve dichiarare assenti pacchetti che non ha potuto verificare: {message}"
    );
}

// --- A5.1-bis: il falso positivo trovato in CI ------------------------------
//
// Ubuntu 24.04, job `native`: `snapshot fallito ... nessun pacchetto installabile
// per il gruppo [libfreetype6-dev]`. Due cause concorrenti, entrambe reali:
//   1. l'indice apt del runner non era aggiornato (nessuno aveva fatto update);
//   2. su noble `libfreetype6-dev` è un nome puramente VIRTUALE — anche con
//      l'indice fresco, `apt-cache policy` risponde `Candidate: (none)`, mentre
//      `apt-get install` lo installa senza battere ciglio.
// Il fix copre entrambe: `apt-get update` nel run di bootstrap, e un rilevamento
// che chiede anche "sapresti installarlo?" prima di dichiararlo assente.

#[test]
fn bootstrap_updates_the_index_so_the_next_step_sees_the_candidates() {
    // La regressione del bug, nella sua forma esatta. Sul runner le utility
    // bootstrap erano GIÀ installate → delta vuoto: se l'update stesse dopo
    // l'uscita anticipata di `run`, l'indice resterebbe stantìo e lo step dei
    // deps boccerebbe pacchetti che esistono. Serve il modello condiviso: due
    // step, un solo sistema.
    let model = SystemModel::new(ModelState {
        // Il runner GitHub ha già git/curl/wget/gettext-base.
        packages: strings(&["git", "curl", "wget", "gettext-base"])
            .into_iter()
            .collect(),
        apt_index_stale: true, // e nessuno ha mai fatto apt-get update
        ..Default::default()
    });
    let c = ctx(false);

    let mut bootstrap = AptPackagesStep::bootstrap_with_ops(model.boxed());
    bootstrap
        .snapshot(&c)
        .expect("lo snapshot del bootstrap non deve morire su un indice stantìo");
    assert!(
        snapshot_of(&bootstrap).delta.is_empty(),
        "scenario del bug: le utility bootstrap sono già presenti, delta vuoto"
    );
    bootstrap
        .run(&c)
        .expect("il run del bootstrap aggiorna l'indice anche con delta vuoto");

    // Da qui in poi l'indice è fresco: lo step dei deps deve risolvere tutto.
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(model.boxed());
    deps.snapshot(&c)
        .expect("dopo l'update i candidati ci sono: nessun falso positivo");
    let snap = snapshot_of(&deps);
    assert!(
        snap.delta.contains(&"libfreetype6-dev".to_string()),
        "il pacchetto che in campo veniva bocciato deve essere risolto: {:?}",
        snap.delta
    );
}

#[test]
fn without_the_update_the_dependencies_step_refuses_instead_of_guessing() {
    // Controprova del test sopra: è davvero l'update di bootstrap a salvare la
    // situazione. Senza, lo step dei deps si ferma — e si ferma con la diagnosi
    // sull'indice, non accusando i pacchetti.
    let model = SystemModel::new(ModelState {
        apt_index_stale: true,
        ..Default::default()
    });
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(model.boxed());

    let message = deps
        .snapshot(&ctx(false))
        .expect_err("senza indice lo step dei deps non può decidere")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "diagnosi sull'indice, non sui nomi: {message}"
    );
}

#[test]
fn the_index_update_lives_in_run_never_in_snapshot() {
    // Vincolo C4, il più importante di questo hotfix: `apt-get update` è una
    // mutazione (scrive in /var/lib/apt/lists) e lo snapshot non muta MAI.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert!(
        !ops_of(&log).contains(&Op::PkgRefreshIndex),
        "lo snapshot deve restare NON mutante: nessun apt-get update. Trovato: {:?}",
        ops_of(&log)
    );

    step.run(&c).expect("run");
    let ops = ops_of(&log);
    let update = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRefreshIndex))
        .expect("il run del bootstrap deve aggiornare l'indice");
    let install = ops
        .iter()
        .position(|o| matches!(o, Op::PkgInstall(_)))
        .expect("e poi installare");
    assert!(
        update < install,
        "l'update va prima dell'install, non dopo: {ops:?}"
    );
}

#[test]
fn only_bootstrap_updates_the_index() {
    // L'update serve una volta, dal primo step apt. Ripeterlo a ogni step
    // sarebbe solo tempo perso su un'operazione di rete.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    let c = ctx(false);

    deps.snapshot(&c).expect("snapshot");
    deps.run(&c).expect("run");
    assert!(
        !ops_of(&log).contains(&Op::PkgRefreshIndex),
        "install-system-dependencies non rifà l'update: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_does_not_touch_the_apt_index() {
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = Context {
        dry_run: true,
        ..Default::default()
    };

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(
        ops_of(&log).is_empty(),
        "dry-run: nessun apt-get update, nessuna install. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_third_party_repo_that_fails_does_not_block_the_install() {
    // `apt-get update` esce non-zero anche per UN SOLO repository irraggiungibile,
    // mentre gli indici ufficiali sono arrivati benissimo. Bloccare lì
    // renderebbe l'installer ostaggio di un PPA rotto che non ci riguarda.
    let cfg = MockConfig {
        apt_update_fails: true,
        apt_index_populated: true, // ma l'indice è comunque utilizzabile
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect("un update parziale non deve fermare l'installazione");
    assert!(
        ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "e l'install deve avvenire comunque: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_update_that_leaves_no_index_at_all_is_a_hard_error() {
    // L'altra faccia: se dopo l'update non c'è NESSUN indice (rete assente),
    // proseguire significherebbe far decidere gli step a valle alla cieca.
    let cfg = MockConfig {
        apt_update_fails: true,
        apt_index_populated: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let message = step
        .run(&c)
        .expect_err("senza indice l'installazione non può procedere")
        .to_string();
    assert!(
        message.contains("apt-get update") && message.contains("indice"),
        "il messaggio deve spiegare cosa manca: {message}"
    );
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "e non si installa nulla dopo l'errore: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_real_name_beats_a_virtual_one_because_only_the_real_one_is_purgeable() {
    // Su noble `libfreetype6-dev` è virtuale: `apt-get install` lo accetta, ma
    // dpkg non lo conosce e `apt-get purge libfreetype6-dev` esce 0 rimuovendo
    // ZERO pacchetti. Un delta con quel nome mentirebbe: il rollback direbbe di
    // aver purgato e `libfreetype-dev` resterebbe installato.
    let cfg = MockConfig {
        virtual_packages: installed(&["libfreetype6-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libfreetype6-dev", "libfreetype-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        snapshot_of(&step).delta,
        strings(&["libfreetype-dev"]),
        "vince il nome REALE, non quello virtuale"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libfreetype-dev"]))),
        "e l'undo purga qualcosa che dpkg conosce davvero: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_virtual_only_group_is_installed_rather_than_refused() {
    // Nessun nome reale nel gruppo, ma apt sa installarlo: rifiutare sarebbe il
    // falso positivo di campo. Si procede (con un warning nei log), perché
    // un'installazione bloccata è un danno certo e il residuo un rischio.
    let cfg = MockConfig {
        virtual_packages: installed(&["libfreetype6-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libfreetype6-dev"]);
    let c = ctx(false);

    step.snapshot(&c)
        .expect("un nome virtuale installabile non è un pacchetto assente");
    assert_eq!(snapshot_of(&step).delta, strings(&["libfreetype6-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libfreetype6-dev"]))),
        "apt riceve il nome virtuale, che sa risolvere: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_canonical_list_declares_the_real_name_for_the_virtual_one() {
    // Guardia sulla lista: `libfreetype6-dev` da solo, su noble, porterebbe un
    // nome non purgabile nel delta. Serve il nome reale come alternativa.
    let catalog = AptBackend.catalog();
    let group = catalog
        .odoo_specs()
        .into_iter()
        .find(|s| s.alternatives().iter().any(|n| n == "libfreetype6-dev"))
        .expect("'libfreetype6-dev' deve restare in lista");
    assert!(
        group.alternatives().iter().any(|n| n == "libfreetype-dev"),
        "il nome reale deve essere fra le alternative, trovato {:?}",
        group.alternatives()
    );
}

// --- A-R6-1: il refactor della lista non deve perdere pacchetti -------------
//
// Il refactor R6 (nomi secchi → gruppi di alternative) ha riscritto a mano una
// lista di 30 pacchetti. Un refactor così è esattamente il posto dove un
// pacchetto si perde in silenzio: i 215 test su mock non creano un venv reale né
// compilano nulla, quindi la mancanza si manifesterebbe solo in campo, a
// installazione avviata. Queste guardie stanno a livello di **lista**, dove il
// refactor avviene, e non hanno bisogno di un sistema vero per fallire.

/// L'insieme obbligatorio **prima** di R6 (`git show c120089`), verbatim.
///
/// Congelato di proposito: è il riferimento contro cui misurare ogni futura
/// riscrittura della lista. Se un pacchetto sparisce, il test qui sotto dice
/// *quale*, senza aspettare la CI reale.
const PRE_R6_REQUIRED: &[&str] = &[
    "git",
    "curl",
    "wget",
    "python3-pip",
    "python3-dev",
    "python3-venv",
    "python3-wheel",
    "python3-setuptools",
    "build-essential",
    "gettext-base",
    "libfreetype6-dev",
    "libxml2-dev",
    "libzip-dev",
    "libldap2-dev",
    "libsasl2-dev",
    "node-less",
    "libjpeg-dev",
    "zlib1g-dev",
    "libpq-dev",
    "libxslt1-dev",
    "libtiff5-dev",
    "libjpeg8-dev",
    "libopenjp2-7-dev",
    "liblcms2-dev",
    "libwebp-dev",
    "libharfbuzz-dev",
    "libfribidi-dev",
    "libxcb1-dev",
    "libev-dev",
    "libc-ares-dev",
];

/// I pacchetti pre-R6 **volutamente** non più obbligatori, con la ragione.
/// Ogni voce qui è una decisione documentata in R6, non una perdita.
const INTENTIONALLY_DEMOTED: &[&str] = &[
    // Rimosso da alcune release Debian; serve solo a compilare asset .less, che
    // Odoo moderno non usa. Vive in ODOO_OPTIONAL_DEPENDENCIES.
    "node-less",
];

/// Tutti i nomi che compaiono nei gruppi **obbligatori** del catalogo Debian
/// (bootstrap + dipendenze Odoo).
///
/// Legge il catalogo del backend invece di una costante: da M1 la lista è ciò
/// che il gestore risponde, e un test che guardasse ancora una costante
/// verificherebbe qualcosa che nessuno consuma più.
fn required_names() -> HashSet<String> {
    let catalog = AptBackend.catalog();
    catalog
        .bootstrap_specs()
        .into_iter()
        .chain(catalog.odoo_specs())
        .filter(|spec| spec.is_required())
        .flat_map(|spec| spec.alternatives().to_vec())
        .collect()
}

/// I nomi dei gruppi **opzionali** del catalogo Debian.
fn optional_names() -> Vec<String> {
    AptBackend
        .catalog()
        .odoo_specs()
        .into_iter()
        .filter(|spec| !spec.is_required())
        .flat_map(|spec| spec.alternatives().to_vec())
        .collect()
}

#[test]
fn the_refactor_did_not_lose_a_single_package() {
    // La guardia che chiude la classe di bug, non l'istanza: ogni pacchetto che
    // era obbligatorio prima di R6 deve essere ancora raggiungibile in un gruppo
    // obbligatorio, oppure comparire fra i declassamenti espliciti.
    let required = required_names();
    let optional: HashSet<String> = optional_names().into_iter().collect();

    let mut lost = Vec::new();
    for pkg in PRE_R6_REQUIRED {
        let demoted = INTENTIONALLY_DEMOTED.contains(pkg);
        if required.contains(*pkg) {
            assert!(
                !demoted,
                "'{pkg}' è marcato come declassato ma è ancora obbligatorio: \
                 aggiorna INTENTIONALLY_DEMOTED o la lista"
            );
            continue;
        }
        if demoted {
            assert!(
                optional.contains(*pkg),
                "'{pkg}' è dichiarato declassato ma non è nemmeno fra gli opzionali"
            );
            continue;
        }
        lost.push(*pkg);
    }

    assert!(
        lost.is_empty(),
        "pacchetti persi rispetto a prima di R6: {lost:?}. \
         Se la rimozione è voluta, dichiarala in INTENTIONALLY_DEMOTED con la ragione"
    );
}

#[test]
fn the_python3_core_set_is_complete() {
    // pip/dev/venv/wheel/setuptools sono un insieme coeso: perderne uno rompe il
    // venv o la compilazione delle wheel, e il sintomo compare step più avanti
    // (create-virtualenv, o pip che non compila) dove è difficile ricondurlo alla
    // lista dei pacchetti. `python3-venv` in particolare è quello senza cui
    // `python3 -m venv` lascia una sandbox senza `bin/python` (A-R6-1).
    let required = required_names();
    for pkg in [
        "python3-pip",
        "python3-dev",
        "python3-venv",
        "python3-wheel",
        "python3-setuptools",
    ] {
        assert!(
            required.contains(pkg),
            "'{pkg}' deve stare fra le dipendenze OBBLIGATORIE: {:?}",
            required
                .iter()
                .filter(|p| p.starts_with("python3"))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn python3_venv_is_never_optional() {
    // Vincolo esplicito: senza venv non c'è Odoo. Deve essere impossibile
    // declassarlo per sbaglio nella lista degli opzionali.
    let optional = optional_names();
    assert!(
        !optional.iter().any(|p| p.contains("venv")),
        "nessun pacchetto venv può stare fra gli opzionali: {optional:?}"
    );
}

#[test]
fn python3_venv_reaches_apt_whether_or_not_it_is_already_installed() {
    // Il test di lista dice "c'è nella lista". Questo dice "arriva ad apt", che è
    // la proprietà che conta — e copre l'osservazione che ha fatto sospettare la
    // perdita: sul runner `python3-venv` NON era nel delta perché era GIÀ
    // installato, non perché mancasse. Entrambi i rami vanno verificati.
    for already_present in [false, true] {
        let cfg = MockConfig {
            installed_packages: if already_present {
                installed(&["python3-venv"])
            } else {
                HashSet::new()
            },
            ..Default::default()
        };
        let (mock, log) = MockSystemOps::new(cfg);
        let mut step = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
        let c = ctx(false);

        step.snapshot(&c).expect("snapshot");
        let snap = snapshot_of(&step);
        let seen = snap.delta.contains(&"python3-venv".to_string())
            || snap.already_installed.contains(&"python3-venv".to_string());
        assert!(
            seen,
            "python3-venv deve risultare nello snapshot (delta o già installato), \
             già presente = {already_present}: delta={:?}",
            snap.delta
        );

        step.run(&c).expect("run");
        let installed_line = ops_of(&log)
            .into_iter()
            .find_map(|o| match o {
                Op::PkgInstall(pkgs) => Some(pkgs),
                _ => None,
            })
            .expect("il run deve invocare apt-get install");
        assert!(
            installed_line.contains(&"python3-venv".to_string()),
            "python3-venv deve essere nella riga di apt anche quando è già presente \
             (apt è idempotente), già presente = {already_present}: {installed_line:?}"
        );
    }
}

#[test]
fn apt_cache_stats_output_is_parsed_as_apt_prints_it() {
    // Output reale di `apt-cache stats` (Ubuntu 24.04) e il caso che conta: un
    // indice vuoto, che è ciò che distingue "non lo so" da "non esiste".
    let populated =
        "Total package names: 163333 (4573 k)\nTotal package structures: 148622 (6539 k)\n";
    assert_eq!(total_package_names(populated), Some(163333));
    assert_eq!(
        total_package_names("Total package names: 0 (0 B)\n"),
        Some(0)
    );
    assert_eq!(
        total_package_names("E: Impossibile aprire il file di lock\n"),
        None,
        "output senza la riga → non lo sappiamo, che NON è zero"
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
        ops_of(&log).contains(&Op::PkgInstall(strings(&["build-essential"]))),
        "il pacchetto disponibile va installato comunque: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_canonical_list_declares_the_renames_seen_in_the_field() {
    // Guardia sulla lista di produzione: i due nomi che hanno rotto Debian 12 in
    // R5 devono avere un'alternativa. Senza questo test, una futura pulizia
    // della lista potrebbe riportarli a nomi secchi e riaprire A5.1 in silenzio.
    let specs = AptBackend.catalog().odoo_specs();
    for broken in ["libtiff5-dev", "libjpeg8-dev"] {
        let group = specs
            .iter()
            .find(|s| s.alternatives().iter().any(|n| n == broken))
            .unwrap_or_else(|| panic!("'{broken}' deve restare in lista"));
        assert!(
            group.alternatives().len() > 1,
            "'{broken}' non esiste su Debian 12: il suo gruppo deve avere un'alternativa, \
             trovato {:?}",
            group.alternatives()
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
