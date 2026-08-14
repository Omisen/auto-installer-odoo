//! [`PatchBashrc`]: the surgical `.bashrc` mutation (C3).

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::patch_bashrc::{remove_exact_line, PatchBashrc};

const PATH_LINE: &str = r#"export PATH="$HOME/.local/bin:$PATH""#;

/// the home is injected into the mock; the context carries the user.
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
    // THE critical test: after run and undo the file is IDENTICAL to the
    // original, aliases and functions intact.
    let dir = tempfile::tempdir().expect("tempdir");
    let bashrc = dir.path().join(".bashrc");
    let original = "alias ll='ls -la'\nfunction greet() { echo hi; }\nexport EDITOR=vim\n";
    std::fs::write(&bashrc, original).expect("write original");

    let (mock, log) = MockSystemOps::new(cfg_home(dir.path()));
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = ctx_home();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // the run only appended, never rewrote the whole file.
    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(o, Op::AppendLine(_))),
        "the run must append the line"
    );
    assert!(
        !ops.iter().any(|o| matches!(o, Op::WritePrivateFile(_))),
        "never rewrite the whole .bashrc"
    );
    // after the run our line is there.
    let after_run = std::fs::read_to_string(&bashrc).expect("read");
    assert!(after_run.contains(PATH_LINE));
    assert!(
        after_run.starts_with(original),
        "the original contents stay at the top, intact"
    );

    step.undo(&c).expect("undo");

    let after_undo = std::fs::read_to_string(&bashrc).expect("read");
    assert_eq!(
        after_undo, original,
        "after the undo the .bashrc is identical to the original"
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

    // no append, so no duplicates and no change to the user's file.
    assert!(!ops_of(&log).iter().any(|o| matches!(o, Op::AppendLine(_))));
    assert_eq!(
        std::fs::read_to_string(&bashrc).expect("read"),
        content,
        "a pre-existing line means an unchanged file"
    );
}

#[test]
fn created_bashrc_is_removed_on_undo() {
    // a missing file is created with the line, and removed by the undo.
    let dir = tempfile::tempdir().expect("tempdir");
    let bashrc = dir.path().join(".bashrc");
    assert!(!bashrc.exists());

    let (mock, _log) = MockSystemOps::new(cfg_home(dir.path()));
    let mut step = PatchBashrc::with_ops(Box::new(mock));
    let c = ctx_home();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(bashrc.exists(), "the run creates the missing .bashrc");

    step.undo(&c).expect("undo");
    assert!(!bashrc.exists(), "the undo removes the .bashrc we created");
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
        "without SUDO_USER the step must fail"
    );
}

#[test]
fn remove_exact_line_is_not_fuzzy() {
    // a DIFFERENT handwritten PATH line is NOT removed.
    let content = format!("alias x='y'\nexport PATH=\"$HOME/bin:$PATH\"\n{PATH_LINE}\n");
    let cleaned = remove_exact_line(&content, PATH_LINE);

    assert!(!cleaned.contains(PATH_LINE), "our exact line is removed");
    assert!(
        cleaned.contains(r#"export PATH="$HOME/bin:$PATH""#),
        "the user's different PATH line stays (exact match, not partial)"
    );
    assert!(cleaned.contains("alias x='y'"));
    assert_eq!(cleaned, "alias x='y'\nexport PATH=\"$HOME/bin:$PATH\"\n");
}
