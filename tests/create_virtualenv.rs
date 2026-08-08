//! Test di [`CreateVirtualenv`] (Fase 6).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::create_virtualenv::CreateVirtualenv;

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn venv_dir() -> PathBuf {
    PathBuf::from("/opt/odoo/odoo18/sandbox")
}

#[test]
fn absent_creates_and_undo_removes() {
    let cfg = MockConfig {
        venv_exists: false,
        venv_available: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.contains(&Op::CreateVenv {
        python: "python3".to_string(),
        venv: venv_dir()
    }));
    assert!(
        ops.contains(&Op::RemoveDirAll(venv_dir())),
        "undo: rm -rf del venv"
    );
}

#[test]
fn preexisting_venv_is_noop() {
    let cfg = MockConfig {
        venv_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "un venv preesistente non va né creato né rimosso"
    );
}

#[test]
fn missing_python_venv_is_error() {
    let cfg = MockConfig {
        venv_exists: false,
        venv_available: false, // python3-venv assente
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let message = step
        .run(&c)
        .expect_err("senza python3-venv il run deve fallire")
        .to_string();

    // Il messaggio deve dire cosa manca e come rimediare: è l'errore che l'utente
    // vede al posto di un `python3 -m venv` che si ferma a metà (A-R6-1).
    assert!(
        message.contains("ensurepip") && message.contains("python3-venv"),
        "l'errore deve nominare il modulo mancante e il pacchetto: {message}"
    );
    // E soprattutto: si ferma PRIMA di creare la sandbox parziale.
    assert!(
        ops_of(&log).is_empty(),
        "nessuna mutazione se la precondizione non è soddisfatta: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_venv_precondition_asks_about_ensurepip_not_about_the_venv_module() {
    // A-R6-1, la ragione per cui la precondizione non poteva fallire.
    //
    // Il mock risponde a `python_venv_available` con un bool, quindi nessun test
    // su mock può accorgersi che l'implementazione reale stia ponendo la domanda
    // sbagliata. Qui si verifica la sostanza: il modulo `venv` vive in
    // `libpython3.x-stdlib` e risponde sempre (`python3 -m venv --help` → 0),
    // mentre `ensurepip` — senza cui la creazione si ferma a metà — arriva col
    // pacchetto `python3-venv`. Chiedere del primo è un controllo che passa
    // sempre; chiedere del secondo è il controllo vero.
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("system_ops.rs"),
    )
    .expect("system_ops.rs leggibile");

    // La dichiarazione nel trait finisce con `;`, quindi la graffa compare solo
    // nell'implementazione: quel che segue è il corpo.
    let body = source
        .split("fn python_venv_available(&self, python: &str) -> bool {")
        .nth(1)
        .expect("l'implementazione di python_venv_available deve esistere");
    let body = body.split("\n    }").next().expect("corpo del metodo");

    assert!(
        body.contains("import ensurepip"),
        "la precondizione deve interrogare ensurepip: {body}"
    );
    assert!(
        !body.contains("\"--help\""),
        "e NON `venv --help`, che risponde 0 anche senza il pacchetto python3-venv: {body}"
    );
    // M11: e la domanda va posta all'interprete che verrà usato davvero, non a
    // `python3` cablato. Su una Fedora ≥ 43 i due divergono, e chiedere al primo
    // darebbe di nuovo una risposta giusta alla domanda sbagliata.
    assert!(
        body.contains("Command::new(python)"),
        "la precondizione deve interrogare l'interprete scelto, non `python3`: {body}"
    );
}
