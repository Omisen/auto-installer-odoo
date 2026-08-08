//! Test di [`PatchBashrc`] (Fase 10): la mutazione chirurgica del .bashrc (C3).

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::patch_bashrc::{remove_exact_line, PatchBashrc};

const PATH_LINE: &str = r#"export PATH="$HOME/.local/bin:$PATH""#;

/// La home è iniettata nel mock via `cfg.sudo_home`; il Context porta l'utente.
fn ctx_home() -> Context {
    Context {
        sudo_user: Some("alice".to_string()),
        dry_run: false,
        ..Default::default()
    }
}

fn cfg_home(home: &std::path::Path) -> MockConfig {
    MockConfig {
        sudo_home: Some(home.to_string_lossy().into_owned()),
        real_fs: true,
        ..Default::default()
    }
}

#[test]
fn round_trip_restores_file_byte_for_byte() {
    // IL test critico: dopo run+undo il .bashrc è IDENTICO all'originale, con
    // alias e funzioni dell'utente intatti.
    let dir = tempfile::tempdir().expect("tempdir");
    let bashrc = dir.path().join(".bashrc");
    let original = "alias ll='ls -la'\nfunction greet() { echo hi; }\nexport EDITOR=vim\n";
    std::fs::write(&bashrc, original).expect("write original");

    let (mock, log) = MockSystemOps::new(cfg_home(dir.path()));
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = ctx_home();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // Il run ha SOLO appeso (mai riscritto l'intero file).
    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(o, Op::AppendLine(_))),
        "run deve appendere la riga"
    );
    assert!(
        !ops.iter().any(|o| matches!(o, Op::WritePrivateFile(_))),
        "mai riscrivere il .bashrc intero"
    );
    // Dopo il run la nostra riga c'è.
    let after_run = std::fs::read_to_string(&bashrc).expect("read");
    assert!(after_run.contains(PATH_LINE));
    assert!(
        after_run.starts_with(original),
        "il contenuto originale resta in testa, intatto"
    );

    step.undo(&c).expect("undo");

    let after_undo = std::fs::read_to_string(&bashrc).expect("read");
    assert_eq!(
        after_undo, original,
        "dopo l'undo il .bashrc è identico all'originale"
    );
}

#[test]
fn line_already_present_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bashrc = dir.path().join(".bashrc");
    let content = format!("alias x='y'\n{PATH_LINE}\n");
    std::fs::write(&bashrc, &content).expect("write");

    let (mock, log) = MockSystemOps::new(cfg_home(dir.path()));
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = ctx_home();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    // Nessun append (niente duplicati), nessuna modifica del file dell'utente.
    assert!(!ops_of(&log).iter().any(|o| matches!(o, Op::AppendLine(_))));
    assert_eq!(
        std::fs::read_to_string(&bashrc).expect("read"),
        content,
        "riga preesistente: file invariato"
    );
}

#[test]
fn created_bashrc_is_removed_on_undo() {
    // .bashrc inesistente → lo creiamo con la riga → undo lo rimuove.
    let dir = tempfile::tempdir().expect("tempdir");
    let bashrc = dir.path().join(".bashrc");
    assert!(!bashrc.exists());

    let (mock, _log) = MockSystemOps::new(cfg_home(dir.path()));
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = ctx_home();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(bashrc.exists(), "il run crea il .bashrc mancante");

    step.undo(&c).expect("undo");
    assert!(!bashrc.exists(), "undo rimuove il .bashrc creato da noi");
}

#[test]
fn missing_sudo_user_is_error() {
    let (mock, _log) = MockSystemOps::new(MockConfig::default());
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = Context {
        sudo_user: None,
        ..Default::default()
    };
    assert!(
        step.snapshot(&c).is_err(),
        "senza SUDO_USER lo step deve fallire"
    );
}

#[test]
fn remove_exact_line_is_not_fuzzy() {
    // Una riga PATH DIVERSA scritta a mano dall'utente NON viene rimossa.
    let content = format!("alias x='y'\nexport PATH=\"$HOME/bin:$PATH\"\n{PATH_LINE}\n");
    let cleaned = remove_exact_line(&content, PATH_LINE);

    assert!(
        !cleaned.contains(PATH_LINE),
        "la nostra riga esatta è rimossa"
    );
    assert!(
        cleaned.contains(r#"export PATH="$HOME/bin:$PATH""#),
        "la riga PATH diversa dell'utente resta (match esatto, non parziale)"
    );
    assert!(cleaned.contains("alias x='y'"));
    assert_eq!(cleaned, "alias x='y'\nexport PATH=\"$HOME/bin:$PATH\"\n");
}
