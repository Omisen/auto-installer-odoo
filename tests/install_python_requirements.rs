//! Test di [`InstallPythonRequirements`] (Fase 6): undo no-op + workaround gevent.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::install_python_requirements::{
    filter_out_gevent_stack, gevent_stack_lines, InstallPythonRequirements,
};

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

const REQUIREMENTS: &str = "gevent==21.12.0 ; sys_platform != 'win32'\npytz\nBabel==2.9.1\n";

/// Estratto **verbatim** dal `requirements.txt` di Odoo 18 (righe 23-31),
/// commenti di Odoo inclusi. È il fixture che conta: il bug A-R6-3 non era una
/// svista di parsing ma il non aver visto che qui le versioni sono **quattro**,
/// una per release di Python — e Odoo lo annota pure.
const ODOO18_REQUIREMENTS: &str = "\
psycopg2==2.9.9\n\
gevent==21.8.0 ; sys_platform != 'win32' and python_version == '3.10'  # (Jammy)\n\
gevent==22.10.2; sys_platform != 'win32' and python_version > '3.10' and python_version < '3.12'\n\
gevent==24.2.1 ; sys_platform != 'win32' and python_version >= '3.12' and python_version < '3.13'  # (Noble)\n\
gevent==24.11.1 ; sys_platform != 'win32' and python_version >= '3.13'  # (Trixie)\n\
greenlet==1.1.2 ; sys_platform != 'win32' and python_version == '3.10'  # (Jammy)\n\
greenlet==2.0.2 ; sys_platform != 'win32' and python_version > '3.10' and python_version < '3.12'\n\
greenlet==3.0.3 ; sys_platform != 'win32' and python_version >= '3.12' and python_version < '3.13' # (Noble)\n\
Babel==2.9.1\n";

/// Estrae gli argomenti delle sole RunAsUser (le install pip), in ordine.
fn pip_calls(ops: &[Op]) -> Vec<Vec<String>> {
    ops.iter()
        .filter_map(|o| match o {
            Op::RunAsUser { program, args, .. } if program.ends_with("pip") => Some(args.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn undo_is_noop_pip_removal_belongs_to_venv() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    let after_run = ops_of(&log).len();

    step.undo(&c).expect("undo");
    let after_undo = ops_of(&log).len();

    // L'undo non esegue NULLA: nessuna disinstallazione, nessun rm.
    assert_eq!(after_run, after_undo, "9c.undo deve essere no-op");
    assert!(!ops_of(&log)
        .iter()
        .any(|o| matches!(o, Op::RemoveDirAll(_))));
}

#[test]
fn gevent_cython_workaround_sequence() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert_eq!(calls.len(), 4, "quattro passaggi pip attesi");

    // 1) upgrade pip wheel setuptools
    assert!(calls[0].contains(&"--upgrade".to_string()) && calls[0].contains(&"pip".to_string()));
    // 2) Cython<3
    assert!(calls[1].contains(&"Cython<3".to_string()));
    // 3) gevent con --no-build-isolation, da file di requirements
    assert!(calls[2].contains(&"--no-build-isolation".to_string()));
    assert!(calls[2].contains(&"--requirement".to_string()));
    // 4) resto dei requirements, --prefer-binary, senza gevent
    assert!(calls[3].contains(&"--prefer-binary".to_string()));
    assert!(calls[3].contains(&"--requirement".to_string()));
    assert!(!calls[3].iter().any(|a| a.contains("gevent")));
}

#[test]
fn setuptools_is_seeded_in_the_venv_before_the_no_build_isolation_step() {
    // A-R6-2, il blocco di Ubuntu 24.04. Da Python 3.12 `venv` semina solo pip:
    // niente setuptools. Il passo 3 usa `--no-build-isolation`, cioè costruisce
    // gevent con quello che trova NEL VENV — e senza setuptools pip muore con
    // `BackendUnavailable: Cannot import 'setuptools.build_meta'`. Il
    // `python3-setuptools` di sistema non serve: il venv è isolato.
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert!(
        calls[0].contains(&"setuptools".to_string()),
        "il bootstrap del venv deve installare setuptools: {:?}",
        calls[0]
    );

    // E deve avvenire PRIMA del passo con --no-build-isolation, altrimenti non
    // serve a nulla.
    let no_isolation = calls
        .iter()
        .position(|c| c.contains(&"--no-build-isolation".to_string()))
        .expect("il passo gevent senza isolamento deve esistere");
    assert!(
        no_isolation > 0,
        "setuptools va seminato prima del build senza isolamento: {calls:?}"
    );
}

#[test]
fn every_pip_call_caches_inside_our_perimeter() {
    // A-R5-3: senza `--cache-dir`, pip scrive in `$HOME/.cache` — e l'`$HOME` di
    // `odoo` è `/opt/odoo`, che è `Preexisting` e che il rollback non svuota. La
    // cache va dentro il venv, che l'undo di CreateVirtualenv rimuove per intero.
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert!(!calls.is_empty(), "il run deve invocare pip");
    let expected = "/opt/odoo/odoo18/sandbox/.pip-cache".to_string();
    for (i, call) in calls.iter().enumerate() {
        let pos = call
            .iter()
            .position(|a| a == "--cache-dir")
            .unwrap_or_else(|| panic!("la chiamata pip #{i} deve passare --cache-dir: {call:?}"));
        assert_eq!(
            call.get(pos + 1),
            Some(&expected),
            "la cache di pip #{i} deve stare dentro il venv, non nella home di odoo"
        );
    }
}

#[test]
fn missing_requirements_is_error() {
    let cfg = MockConfig {
        requirements_content: None, // requirements.txt assente
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "requirements.txt mancante → errore");
    assert!(
        pip_calls(&ops_of(&log)).is_empty(),
        "nessuna install se manca requirements"
    );
}

// --- A-R6-3: la versione di gevent la sceglie pip -------------------------
//
// Odoo 18 pinna quattro gevent e cinque greenlet, uno per versione di Python.
// Estrarre "la prima riga che inizia con gevent" dava la riga di Jammy su
// qualunque sistema: giusta per coincidenza su 22.04, e su 24.04 una versione
// che non compila contro Python 3.12. La correzione è smettere di scegliere.

#[test]
fn every_pinned_version_survives_with_its_marker() {
    // La proprietà che rende il fix un fix: nell'input di pip ci sono TUTTE le
    // versioni, ciascuna col suo marker. Se ne resta una sola, qualcuno ha
    // ricominciato a scegliere al posto di pip.
    let lines = gevent_stack_lines(ODOO18_REQUIREMENTS);

    for version in ["21.8.0", "22.10.2", "24.2.1", "24.11.1"] {
        assert!(
            lines.contains(&format!("gevent=={version}")),
            "manca gevent=={version}: pip non può più scegliere. Prodotto:\n{lines}"
        );
    }
    for version in ["1.1.2", "2.0.2", "3.0.3"] {
        assert!(
            lines.contains(&format!("greenlet=={version}")),
            "manca greenlet=={version}: pip risolverebbe una versione qualunque \
             compatibile con la metadata di gevent, ed è così che greenlet 1.1.x \
             è finito a compilare contro Python 3.12. Prodotto:\n{lines}"
        );
    }

    // E ogni riga si porta dietro il marker: è l'unica cosa che distingue la
    // versione giusta da una che non compila.
    for line in lines.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            line.contains("python_version"),
            "riga senza marker d'ambiente, pip non saprebbe quale scegliere: {line}"
        );
    }
}

#[test]
fn the_complement_keeps_everything_else_and_nothing_of_the_stack() {
    let filtered = filter_out_gevent_stack(ODOO18_REQUIREMENTS);
    assert!(
        !filtered.to_lowercase().contains("gevent")
            && !filtered.to_lowercase().contains("greenlet"),
        "il passo 4 non deve reinstallare ciò che il passo 3 ha già messo: {filtered}"
    );
    assert!(filtered.contains("psycopg2==2.9.9"));
    assert!(filtered.contains("Babel==2.9.1"));
}

#[test]
fn a_similarly_named_package_is_not_mistaken_for_the_stack() {
    // `gevent-websocket` non è `gevent`: il confine dopo il nome esiste per
    // questo, e senza, il passo 3 se lo porterebbe dietro fuori isolamento.
    let requirements = "gevent-websocket==0.10.1\ngreenlet-stubs==1.0\ngevent==24.2.1 ; python_version >= '3.12'\n";
    let lines = gevent_stack_lines(requirements);
    assert!(lines.contains("gevent==24.2.1"));
    assert!(
        !lines.contains("gevent-websocket") && !lines.contains("greenlet-stubs"),
        "catturati pacchetti con nome simile: {lines}"
    );
    assert!(filter_out_gevent_stack(requirements).contains("gevent-websocket==0.10.1"));
}

#[test]
fn requirements_without_gevent_produce_no_dedicated_step() {
    // Niente gevent → il passo 3 non ha ragione di esistere: tre chiamate pip,
    // non quattro, e nessun file temporaneo inutile.
    let cfg = MockConfig {
        requirements_content: Some("pytz\nBabel==2.9.1\n".to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert_eq!(calls.len(), 3, "atteso nessun passo gevent: {calls:?}");
    assert!(
        gevent_stack_lines("pytz\nBabel==2.9.1\n").is_empty(),
        "senza gevent la selezione è vuota, non un default inventato"
    );
}

#[test]
fn pip_receives_a_file_never_a_hand_picked_version() {
    // Il controllo sul comportamento, non solo sulla funzione pura: nessun
    // argomento di pip deve essere una versione di gevent decisa da noi.
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    let gevent_call = &calls[2];
    assert!(
        gevent_call.contains(&"--requirement".to_string()),
        "il passo gevent deve passare un file, così pip valuta i marker: {gevent_call:?}"
    );
    assert!(
        !gevent_call.iter().any(|a| a.starts_with("gevent==")),
        "nessuna versione scelta da noi su argv: {gevent_call:?}"
    );
    assert!(
        gevent_call.contains(&"--no-build-isolation".to_string()),
        "il workaround Cython<3 resta: su Jammy gevent 21.8.0 non ha wheel"
    );
}
