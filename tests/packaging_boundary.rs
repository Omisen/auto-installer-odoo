//! M1 — l'estrazione dei due confini, e le due cose che l'estrazione ha
//! cambiato davvero.
//!
//! Il grosso di M1 è un refactor: la garanzia che sia stato neutro sono i 324
//! test già esistenti, che passano invariati. Qui stanno i test di ciò che
//! **non** è invariato:
//!
//! - la scelta del backend, che ora è una decisione presa in un posto solo e
//!   che può dire di no;
//! - la deduplica dei nomi risolti (A-MD-1), che chiude un difetto trovato
//!   scrivendo il design;
//! - la decisione di disponibilità, estratta come funzione pura perché la
//!   validazione per mutazione l'ha trovata scoperta;
//! - la distinzione fra i costruttori di produzione e quelli dei test, che
//!   togliendo `Step::new()` stava per andare persa.
//!
//! Ciò che l'estrazione **non** ha cambiato — la sequenza del recupero, il
//! pattern delta, le protezioni critiche — resta presidiato dai test che c'erano
//! già, e il fatto che passino invariati è la garanzia che cerchiamo.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::distro::OsFamily;
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::{dedup_keeping_order, AptPackagesStep, UndoPolicy};
use odoo_installer::system_ops::backend_factory;

fn ctx() -> Context {
    Context {
        dry_run: false,
        ..Default::default()
    }
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// --- La scelta del backend: una decisione, in un posto solo ------------------

/// La fabbrica dei backend **può dire di no**, ed è il punto.
///
/// Un `match` che desse apt a una famiglia senza backend sarebbe una bugia
/// silenziosa: `apt-get purge` su una macchina senza apt fallisce, l'undo è
/// best-effort, e il rollback dichiarerebbe fatto ciò che non ha fatto. Qui i
/// due rami sono entrambi **veri**: nessuno dei due è un ramo che non può
/// eseguire.
#[test]
fn the_backend_factory_answers_honestly_for_both_families() {
    assert!(
        backend_factory(OsFamily::Debian).is_some(),
        "la famiglia Debian ha apt: dev'esserci una fabbrica"
    );
    assert!(
        backend_factory(OsFamily::Fedora).is_none(),
        "finché il backend dnf non esiste, la risposta onesta è «non ce l'ho»: \
         dare apt a Fedora significherebbe eseguire comandi che quella macchina \
         non ha, e dichiarare rimosso ciò che è rimasto installato"
    );
}

/// Il default della fabbrica è la famiglia di **ogni installazione esistente**.
#[test]
fn the_default_family_is_the_one_every_existing_manifest_describes() {
    assert!(backend_factory(OsFamily::default()).is_some());
}

/// Il catalogo è ciò che il backend risponde: la lista **non** è più una
/// costante che uno step legge per conto suo.
#[test]
fn the_package_lists_come_from_the_backend_catalog() {
    let ops = MockSystemOps::new(MockConfig::default()).0;
    let catalog = odoo_installer::system_ops::SystemOps::packages(&ops).catalog();

    assert!(
        catalog.bootstrap.iter().any(|s| s.preferred() == "git"),
        "il bootstrap della famiglia Debian contiene git"
    );
    assert!(
        catalog
            .odoo
            .iter()
            .any(|s| s.preferred() == "build-essential"),
        "le dipendenze Odoo della famiglia Debian contengono build-essential"
    );
    assert_eq!(catalog.postgres_marker, "postgresql");
    assert_eq!(catalog.nginx, "nginx");
    assert!(
        catalog.postgres.contains(&"postgresql-contrib".to_string()),
        "il server PostgreSQL su Debian sono due pacchetti, non uno"
    );
}

// --- A-MD-1: il delta persistito non contiene doppioni ----------------------

/// La funzione pura, sui casi che contano.
#[test]
fn dedup_keeps_the_first_occurrence_and_the_order() {
    let mut v = names(&["git", "libjpeg-dev", "curl", "libjpeg-dev", "wget"]);
    dedup_keeping_order(&mut v);
    assert_eq!(v, names(&["git", "libjpeg-dev", "curl", "wget"]));

    // `Vec::dedup` non basterebbe: i duplicati non sono consecutivi. Questo è
    // esattamente il caso reale — nella lista Debian i due `libjpeg-dev` sono a
    // sei posizioni di distanza.
    let mut consecutivi = names(&["a", "a", "b"]);
    dedup_keeping_order(&mut consecutivi);
    assert_eq!(consecutivi, names(&["a", "b"]));

    let mut vuoto: Vec<String> = Vec::new();
    dedup_keeping_order(&mut vuoto);
    assert!(vuoto.is_empty());
}

/// **Il difetto, per come si manifesta.** Due gruppi diversi che risolvono allo
/// stesso nome mettevano quel nome **due volte** nel delta persistito.
///
/// Su apt è innocuo — install e purge sono idempotenti — ma il delta è la
/// contabilità di ciò che abbiamo aggiunto e l'unica cosa su cui l'undo è
/// autorizzato ad agire. Una contabilità con una riga doppia è una contabilità
/// sbagliata, e su una famiglia dove più gruppi Debian collassano su un solo
/// nome il doppione smette di essere un caso di bordo.
#[test]
fn two_groups_resolving_to_the_same_name_appear_once_in_the_delta() {
    let (ops, _log) = MockSystemOps::new(MockConfig {
        // `libjpeg8-dev` non esiste su questa "release": il secondo gruppo
        // ricade sulla terza alternativa, che è la stessa del primo gruppo.
        packages_without_candidate: ["libjpeg8-dev", "libjpeg-turbo8-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            odoo_installer::packaging::PackageSpec::one("libjpeg-dev"),
            odoo_installer::packaging::PackageSpec::any(&[
                "libjpeg8-dev",
                "libjpeg-turbo8-dev",
                "libjpeg-dev",
            ]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");

    let snap: odoo_installer::steps::apt_packages::AptDeltaSnapshot =
        serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile");

    assert_eq!(
        snap.delta,
        names(&["libjpeg-dev"]),
        "il delta persistito deve nominare ogni pacchetto UNA volta: è la \
         contabilità su cui l'undo agisce"
    );
}

/// Lo stesso vale per i preesistenti: se il pacchetto c'era già, entrambi i
/// gruppi lo riconoscono, e `already_installed` non deve elencarlo due volte.
#[test]
fn a_preexisting_package_shared_by_two_groups_is_listed_once() {
    let (ops, _log) = MockSystemOps::new(MockConfig {
        installed_packages: ["libjpeg-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            odoo_installer::packaging::PackageSpec::one("libjpeg-dev"),
            odoo_installer::packaging::PackageSpec::any(&["libjpeg8-dev", "libjpeg-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");

    let snap: odoo_installer::steps::apt_packages::AptDeltaSnapshot =
        serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile");

    assert_eq!(snap.already_installed, names(&["libjpeg-dev"]));
    assert!(
        snap.delta.is_empty(),
        "era già installato: non l'abbiamo aggiunto noi, non è nostro da rimuovere"
    );
}

/// E il `run` non chiede al gestore di installare due volte lo stesso nome.
#[test]
fn the_install_command_does_not_repeat_a_package() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        packages_without_candidate: ["libjpeg8-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            odoo_installer::packaging::PackageSpec::one("libjpeg-dev"),
            odoo_installer::packaging::PackageSpec::any(&["libjpeg8-dev", "libjpeg-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.run(&ctx()).expect("run");

    let installs: Vec<Vec<String>> = ops_of(&log)
        .into_iter()
        .filter_map(|op| match op {
            Op::AptInstall(pkgs) => Some(pkgs),
            _ => None,
        })
        .collect();

    assert_eq!(installs.len(), 1, "una sola invocazione");
    assert_eq!(installs[0], names(&["libjpeg-dev"]));
}

// --- La politica di rimozione parla al gestore, non al SystemOps -------------
//
// La **sequenza** del recupero (riparazione → rimozione → riparazione profonda →
// riparazione → rimozione) non è verificata qui: la presidiano già
// `tests/apt_packages.rs::undo_recovers_a_broken_dpkg_before_purging` e
// `undo_retries_the_purge_after_dpkg_configure_all`, che dopo M1 passano
// **invariati** — ed è proprio quella invarianza la prova che l'estrazione è
// stata neutra. Riscriverli qui sposterebbe la garanzia senza aggiungerla.

/// Un delta vuoto non fa partire nessun comando: non c'è niente di nostro da
/// rimuovere, e un `apt-get purge` senza argomenti sarebbe rumore.
#[test]
fn an_empty_delta_asks_the_manager_for_nothing() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        installed_packages: ["pippo"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::custom(
        Box::new(ops),
        "install-system-dependencies",
        names(&["pippo"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.undo(&ctx()).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::AptPurge(_) | Op::AptFixBroken)),
        "nessun pacchetto nel delta: il gestore non va nemmeno invocato"
    );
}

// --- I costruttori di produzione non sono quelli dei test -------------------

/// **La regressione che clippy ha intercettato durante M1**, messa per iscritto.
///
/// `CloneOdooRepo` e `SetupSystemd` hanno due costruttori che *non* sono
/// intercambiabili: `for_run` porta i parametri di produzione (backoff fra i
/// retry del clone, attesa di assestamento del servizio), `with_ops` li azzera
/// perché i test non devono dormire.
///
/// Togliendo i vecchi `new()` la sequenza di produzione rischiava di essere
/// costruita con i costruttori dei test: tre tentativi di clone **istantanei**
/// su una rete che non risponde sono un tentativo solo, e il retry di R2
/// diventerebbe decorativo. Nessun test su mock se ne sarebbe accorto — i mock
/// vogliono proprio sleep 0.
#[test]
fn the_production_sequence_uses_the_production_constructors() {
    let sorgente = std::fs::read_to_string("src/steps/mod.rs").expect("leggo steps/mod.rs");
    let inizio = sorgente
        .find("pub fn build_steps")
        .expect("build_steps esiste");
    let fine = sorgente[inizio..]
        .find("\n}\n")
        .map(|i| inizio + i)
        .expect("fine di build_steps");
    let corpo = &sorgente[inizio..fine];

    for step in ["CloneOdooRepo", "SetupSystemd"] {
        assert!(
            corpo.contains(&format!("{step}::for_run(")),
            "{step} ha parametri di produzione (attese, backoff) che i suoi test azzerano: \
             in `build_steps` va costruito con `for_run`, non con `with_ops`"
        );
    }
}

/// La reidratazione invece usa `with_ops`, ed è corretto: si sta ricostruendo
/// uno step **per annullarlo**, e rimuovere una directory o fermare un servizio
/// non hanno bisogno di attese.
///
/// È la stessa distinzione di A-R8-1: prima di riusare un costruttore per una
/// domanda nuova, chiedersi a quale domanda rispondeva.
#[test]
fn the_rehydration_path_does_not_need_the_production_timings() {
    let make_ops = backend_factory(OsFamily::Debian).expect("backend Debian");
    for name in ["clone-odoo-repo", "setup-systemd"] {
        assert!(
            odoo_installer::steps::step_by_name(name, &make_ops).is_some(),
            "'{name}' dev'essere ricostruibile per l'undo"
        );
    }
}

// --- Il confine non ha cambiato la protezione che presidia ------------------

/// L'undo purga **solo** il delta, anche ora che il purge passa dal gestore.
/// È la protezione del pattern delta, e non è cambiata: qui la si verifica dal
/// lato che conta, cioè i comandi che arrivano davvero al sistema.
#[test]
fn the_undo_still_removes_only_what_we_added() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        installed_packages: ["gia-presente"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::custom(
        Box::new(ops),
        "install-system-dependencies",
        names(&["gia-presente", "aggiunto-da-noi"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.run(&ctx()).expect("run");
    step.undo(&ctx()).expect("undo");

    let purged: Vec<Vec<String>> = ops_of(&log)
        .into_iter()
        .filter_map(|op| match op {
            Op::AptPurge(pkgs) => Some(pkgs),
            _ => None,
        })
        .collect();

    assert_eq!(
        purged,
        vec![names(&["aggiunto-da-noi"])],
        "il pacchetto che il cliente aveva già non si tocca, qualunque sia il gestore"
    );
}

// --- La decisione di disponibilità, separata dai comandi --------------------

/// La regola che protegge A5.1-bis, verificata **senza apt sotto mano**.
///
/// Il codice che esegue `apt-cache policy` e `apt-get install -s` può essere
/// provato solo su una macchina reale; la *decisione* che ne discende no, e
/// senza questa separazione resterebbe fuori da ogni test — come ha mostrato la
/// validazione per mutazione di M1, dove «dichiara reale un nome virtuale»
/// sopravviveva a tutta la suite.
#[test]
fn a_real_candidate_always_beats_a_virtual_name() {
    use odoo_installer::packaging::{availability_from, Availability};

    assert_eq!(availability_from(true, false), Availability::Real);
    assert_eq!(
        availability_from(false, true),
        Availability::VirtualOnly,
        "installabile ma non per questo nome: è un ripiego, e va detto"
    );
    assert_eq!(availability_from(false, false), Availability::Absent);
    assert_eq!(
        availability_from(true, true),
        Availability::Real,
        "se il candidato è reale la risposta è reale, qualunque cosa dica il \
         risolutore: un nome rimovibile batte sempre uno che non lo è"
    );
}
