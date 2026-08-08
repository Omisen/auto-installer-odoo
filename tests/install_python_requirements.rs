//! Test di [`InstallPythonRequirements`] (Fase 6): undo no-op + workaround gevent.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::install_python_requirements::{
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
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

// --- A-V3-3: dove nascono i requirements temporanei --------------------------

/// Estrae i path dei file creati con la primitiva fail-closed.
fn created_private_files(ops: &[Op]) -> Vec<PathBuf> {
    ops.iter()
        .filter_map(|o| match o {
            Op::CreatePrivateFile(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// **Il difetto di A-V3-3.** I due requirements nascevano in `/tmp` con un nome
/// scritto nel sorgente: root li scriveva, pip li leggeva come utente `odoo`, e
/// nella finestra in mezzo chiunque avesse un utente locale poteva sostituirli
/// e far installare pacchetti arbitrari nel venv.
///
/// Ora nascono dentro `<install_dir>/sandbox`, che è di proprietà di `odoo` e
/// non è scrivibile da terzi: il presupposto dell'attacco sparisce, invece di
/// essere contrastato.
#[test]
fn requirements_are_written_inside_the_venv_not_in_a_shared_temp_dir() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let creati = created_private_files(&ops_of(&log));
    assert_eq!(
        creati.len(),
        2,
        "attesi i due file di requirements (gevent + filtrato): {creati:?}"
    );

    let venv = c.install_dir.join("sandbox");
    for path in &creati {
        assert!(
            path.starts_with(&venv),
            "{} deve nascere dentro il venv, non altrove: in /tmp sarebbe sostituibile \
             da un utente locale prima che pip lo legga (A-V3-3)",
            path.display()
        );
        let nome = path
            .file_name()
            .expect("nome")
            .to_string_lossy()
            .into_owned();
        assert!(
            nome.starts_with('.') && nome.ends_with(".tmp"),
            "nome non imprevedibile: {nome}"
        );
    }
    assert_ne!(
        creati[0], creati[1],
        "due file distinti, non lo stesso path"
    );
}

/// Il file nasce `0600 root` e a leggerlo è pip, che gira come `odoo`: senza il
/// `chown` il passo fallirebbe. È l'unico motivo per cui il chown esiste.
#[test]
fn each_requirements_file_is_handed_over_to_the_odoo_user() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    for path in created_private_files(&ops) {
        assert!(
            ops.iter().any(|o| matches!(
                o,
                Op::ChownNamed { path: p, owner, group }
                    if *p == path && owner == "odoo" && group == "odoo"
            )),
            "{} non viene consegnato a odoo: pip non potrebbe leggerlo",
            path.display()
        );
    }
}

/// I temporanei vengono rimossi dopo l'uso, e la rimozione passa da `SystemOps`
/// come la creazione. Restano comunque dentro il venv, quindi anche un'esecuzione
/// interrotta non lascia nulla fuori dal perimetro reversibile.
#[test]
fn requirements_files_are_removed_after_use() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    for path in created_private_files(&ops) {
        assert!(
            ops.iter()
                .any(|o| matches!(o, Op::RemoveFile(p) if *p == path)),
            "{} non viene rimosso dopo l'uso",
            path.display()
        );
    }
}

// --- A-MD-7: quando pip fallisce, dire perché --------------------------------

/// Il fallimento del passo gevent su un Python più recente dei pin di Odoo
/// arriva con la **causa davanti**, e con l'errore originale ancora dietro.
///
/// È il rosso della sonda Fedora 44: Python 3.14, `gevent==24.11.1` (il pin per
/// `>= '3.13'`, l'ultimo che Odoo dichiara), nessuna wheel per quell'interprete
/// → pip compila → trecento righe di `gcc` che parlano di `_PyLong_AsByteArray`.
/// Da quell'output la causa vera non è ricavabile, ed è tutta l'utilità di
/// questa diagnosi: chi legge deve capire che è la **versione**, non l'ambiente
/// di build.
///
/// Si verifica passando dallo `run` dello step, non chiamando la funzione pura:
/// una diagnosi giusta che nessuno invoca è indistinguibile da una assente.
#[test]
fn a_gevent_failure_on_a_newer_python_says_why() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: Some((3, 14)),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("il passo gevent è fallito")
        .to_string();

    assert!(
        err.contains("3.14"),
        "la diagnosi non dice quale Python c'è sotto: {err}"
    );
    assert!(
        err.contains("3.13"),
        "la diagnosi non dice fin dove arriviamo, quindi non si sa di quanto si è avanti: {err}"
    );
    assert!(
        err.contains("gevent==24.11.1"),
        "la diagnosi non mostra i pin che Odoo dichiara: {err}"
    );
    assert!(
        err.contains("Building wheel for gevent"),
        "l'errore originale è sparito: spiegare non è nascondere la prova: {err}"
    );
}

/// Su un Python **coperto** dai pin lo stesso fallimento passa intatto.
///
/// È la metà che rende il controllo un controllo: lì la causa sarà un'altra —
/// un compilatore assente, un header mancante, una rete che cade — e una
/// diagnosi sbagliata è peggio di nessuna diagnosi, perché manda a sistemare la
/// cosa sbagliata (la lezione di A-R9-1, dove il messaggio parlava della porta).
#[test]
fn on_a_covered_python_the_pip_error_is_left_alone() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: Some((3, 12)),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("il passo gevent è fallito")
        .to_string();

    assert!(
        err.contains("Building wheel for gevent"),
        "l'errore di pip deve restare quello che è: {err}"
    );
    assert!(
        !err.contains("più recente di Python"),
        "su un Python coperto la diagnosi di A-MD-7 non c'entra nulla: {err}"
    );
}

/// E se non si sa che Python sia, non si indovina.
///
/// `None` è «non lo so», non «va bene» e nemmeno «è troppo nuovo»: da
/// un'informazione assente non si conclude niente, e l'errore resta quello del
/// comando.
#[test]
fn an_unknown_interpreter_does_not_become_a_guess() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: None,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("il passo gevent è fallito")
        .to_string();

    assert!(
        !err.contains("più recente di Python"),
        "senza sapere la versione non si può affermare che sia troppo nuova: {err}"
    );
}
