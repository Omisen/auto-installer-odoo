//! [`WriteControlScript`]: owned by SUDO_USER, and never global.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::write_control_script::{control_script_content, WriteControlScript};

fn ctx(sudo_user: Option<&str>) -> Context {
    Context {
        sudo_user: sudo_user.map(|s| s.to_string()),
        odoo_user: "odoo".to_string(),
        odoo_version_short: "18".to_string(),
        dry_run: false,
        ..Default::default()
    }
}

/// every path the operations reference, for the "not global" check.
fn all_paths(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .flat_map(|o| match o {
            Op::WritePrivateFile(p)
            | Op::RemoveFile(p)
            | Op::RemoveSymlink(p)
            | Op::Rmdir(p)
            | Op::Chmod { path: p, .. } => vec![p.to_string_lossy().into_owned()],
            Op::MkdirAsUser { path, .. } => vec![path.to_string_lossy().into_owned()],
            Op::CreateSymlink { src, link } => {
                vec![
                    src.to_string_lossy().into_owned(),
                    link.to_string_lossy().into_owned(),
                ]
            }
            Op::ChownToUser { path, .. } => vec![path.to_string_lossy().into_owned()],
            _ => vec![],
        })
        .collect()
}

#[test]
fn absent_creates_owned_by_sudo_user_and_undo_removes() {
    let cfg = MockConfig {
        sudo_home: Some("/home/alice".to_string()),
        path_exists: false,
        our_link_exists: false,
        dir_empty: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(Some("alice"));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    // script and symlink created.
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::WritePrivateFile(p) if p.ends_with(".scripts/odoo.sh"))));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::CreateSymlink { link, .. } if link.ends_with(".local/bin/odoo"))));

    // ownership is SUDO_USER, NEVER `odoo` or root.
    let chowns: Vec<&String> = ops
        .iter()
        .filter_map(|o| match o {
            Op::ChownToUser { user, .. } => Some(user),
            _ => None,
        })
        .collect();
    assert!(!chowns.is_empty());
    assert!(
        chowns.iter().all(|u| *u == "alice"),
        "the owner must be SUDO_USER, found: {chowns:?}"
    );

    // not global: nothing under a system path.
    assert!(
        all_paths(&ops).iter().all(|p| !p.contains("/usr/")),
        "the command is not installed globally"
    );

    // the undo removes our artifacts.
    step.undo(&c).expect("undo");
    let ops = ops_of(&log);
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveSymlink(p) if p.ends_with(".local/bin/odoo"))));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveFile(p) if p.ends_with(".scripts/odoo.sh"))));
}

#[test]
fn preexisting_artifacts_are_not_recreated_or_removed() {
    let cfg = MockConfig {
        sudo_home: Some("/home/alice".to_string()),
        path_exists: true,
        our_link_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(Some("alice"));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);

    // A-V3-9: the script is ALWAYS rewritten. its contents are ours and carry
    // the service name, so skipping it left the helper driving an earlier
    // installation's service.
    assert!(
        ops.iter().any(|o| matches!(o, Op::WritePrivateFile(_))),
        "the script is rewritten: its contents depend on the installed version"
    );
    // but what was there is not destroyed: it is set aside first.
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::CopyFile { src, .. } if src.ends_with(".scripts/odoo.sh"))),
        "a pre-existing script is backed up before being rewritten: {ops:?}"
    );

    assert!(
        !ops.iter().any(|o| matches!(o, Op::CreateSymlink { .. })),
        "a pre-existing symlink is not recreated (it points at the same script anyway)"
    );
    // the undo does not remove what was not ours: it puts it back.
    assert!(
        !ops.iter().any(|o| matches!(o, Op::RemoveFile(_))),
        "we do not remove artifacts that are not ours"
    );
    assert!(!ops.iter().any(|o| matches!(o, Op::RemoveSymlink(_))));
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::MoveFile { dst, .. } if dst.ends_with(".scripts/odoo.sh"))),
        "the undo must put the pre-existing script back: {ops:?}"
    );
}

#[test]
fn missing_sudo_user_is_error() {
    let (mock, _log) = MockSystemOps::new(MockConfig::default());
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(None); // SUDO_USER assente
    assert!(
        step.snapshot(&c).is_err(),
        "without SUDO_USER the step must fail"
    );
}

#[test]
fn script_content_wraps_service_and_user() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    assert!(content.contains("SERVICE_NAME=\"odoo18\""));
    assert!(content.contains("ODOO_OS_USER=\"odoo\""));
    assert!(content.contains("COMMAND_NAME=\"odoo\""));
    assert!(
        content.contains("Usage: ${COMMAND_NAME} "),
        "the usage line must name the command the helper is invoked by"
    );
    assert!(content.contains("systemctl start"));
    assert!(content.contains("systemctl status"));
}

/// the helper drives **its own** instance, and only that one.
///
/// a machine can carry several, each with its own service, user, database and
/// port. A helper that started or stopped somebody else's would take one
/// customer offline to fix another one's problem — the hazard the shared-artifact
/// rule protects the rollback from, arriving through the front door instead.
///
/// so every mutating verb names `${SERVICE_NAME}` and nothing else. This test is
/// the guard on that: it reads the mutating branches and refuses any `systemctl`
/// there that acts on a name we did not derive.
#[test]
fn only_this_instances_service_is_ever_started_or_stopped() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        // the mutating verbs only: `list-units` and `status` read, and they are
        // allowed — that is how the helper shows the machine without touching it.
        for verb in ["systemctl start", "systemctl stop", "systemctl restart"] {
            if let Some(rest) = line.strip_prefix(&format!("sudo {verb}")) {
                assert_eq!(
                    rest.trim(),
                    "\"${SERVICE_NAME}\"",
                    "a mutating systemctl must act on this instance's service and no other: {line}"
                );
            }
        }
    }

    // and the read-only half is there, asked of the right source. `list-units`
    // is the wrong one and a VM proved it: a stopped service is unloaded and
    // disappears from that listing even with `--all` — exactly the instance
    // somebody running `status` is hunting for. The unit FILES are what is
    // installed; `is-active` says what is up.
    assert!(
        content.contains("systemctl list-unit-files"),
        "the instances are enumerated from the unit files, which do not vanish when stopped"
    );
    assert!(
        content.contains("systemctl is-active"),
        "and whether each is up is asked per unit"
    );
    assert!(
        !content.contains("systemctl list-units"),
        "list-units hides a stopped instance: it must not come back"
    );
}

/// `dev` leaves the service stopped, and has to say so.
///
/// it stops the service on purpose — you are about to run `odoo-bin` by hand on
/// the same port — but when the shell exits nothing starts it again, and until
/// now nothing said that either. An instance quietly left down after a debugging
/// session is a defect of the helper, not of whoever used it.
#[test]
fn dev_says_that_it_leaves_the_service_stopped() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    let dev = content
        .split("  dev)")
        .nth(1)
        .expect("the dev branch must exist")
        .split("  status)")
        .next()
        .expect("it ends where the next branch starts");

    assert!(dev.contains("systemctl stop"), "dev stops the service");
    assert!(
        dev.contains("STOPPED") && dev.contains("${COMMAND_NAME} start"),
        "and it must say that it stays stopped, and how to bring it back:\n{dev}"
    );
    assert!(
        !dev.contains("systemctl start"),
        "dev must not restart it by itself: you may want to keep working by hand"
    );
}

/// an unknown verb is an error, not a suggestion.
///
/// it used to print the usage on stdout and exit **0**, so a script calling the
/// helper could not tell a typo from a success.
#[test]
fn an_unknown_verb_fails_and_says_so_on_stderr() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    assert!(
        content.contains("usage >&2") && content.contains("exit 2"),
        "the fallback branch must go to stderr and exit non-zero"
    );
    assert!(
        content.contains("-h|--help|help)"),
        "asking for help is not an error, and must not exit non-zero"
    );
}

/// the generated script is **valid bash**.
///
/// nobody compiles it: it is written to a customer's home and first run by a
/// human, weeks later, when something is already wrong. A syntax error would
/// surface there. The CI scripts have had this guard for the same reason.
#[test]
fn the_generated_script_is_syntactically_valid() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("helper.sh");
    std::fs::write(&path, &content).expect("write");

    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("bash must be available");
    assert!(
        out.status.success(),
        "the generated helper does not parse:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// prints the rendered helper, to look at it: `cargo test --test control_script
/// -- --ignored --nocapture show_the_helper`.
#[test]
#[ignore]
fn show_the_helper() {
    println!(
        "{}",
        control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x")
    );
}
