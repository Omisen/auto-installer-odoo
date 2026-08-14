//! [`CreateVirtualenv`].

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

    // the message must say what is missing and how to fix it: this is what the
    // user sees instead of a venv that stops halfway (A-R6-1).
    assert!(
        message.contains("ensurepip") && message.contains("python3-venv"),
        "l'errore deve nominare il modulo mancante e il pacchetto: {message}"
    );
    // and above all it stops BEFORE creating a partial sandbox.
    assert!(
        ops_of(&log).is_empty(),
        "nessuna mutazione se la precondizione non è soddisfatta: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_venv_precondition_asks_about_ensurepip_not_about_the_venv_module() {
    // A-R6-1: why the precondition could not fail.
    //
    // the mock answers with a bool, so no mock test can notice the real
    // implementation asking the wrong question. this checks the substance: the
    // `venv` module is in the stdlib and always answers, while `ensurepip`
    // comes with a separate package. asking about the first always passes.
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("system_ops.rs"),
    )
    .expect("system_ops.rs leggibile");

    // the trait declaration ends with `;`, so the brace appears only in the
    // implementation: what follows is the body.
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
    // M11: the question goes to the interpreter that will really be used, not a
    // hardcoded one. where the two diverge, asking the wrong one gives the
    // right answer to the wrong question.
    assert!(
        body.contains("Command::new(python)"),
        "la precondizione deve interrogare l'interprete scelto, non `python3`: {body}"
    );
}
