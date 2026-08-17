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
        //
        // the invocation is looked for ANYWHERE in the line, not at its start:
        // the restore wraps one in `if ! … ; then` to report a service that did
        // not come back, and a guard anchored to the left margin would have
        // stopped seeing it — a structural guard that quietly narrows its own
        // reach is worse than none, because the green stays.
        for verb in ["systemctl start", "systemctl stop", "systemctl restart"] {
            if let Some((_, rest)) = line.split_once(&format!("sudo {verb}")) {
                let target = rest
                    .trim()
                    .trim_end_matches("; then")
                    .trim_end_matches(';')
                    .trim();
                assert_eq!(
                    target, "\"${SERVICE_NAME}\"",
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

/// the `dev` branch on its own, without the verbs around it.
fn dev_branch(content: &str) -> String {
    content
        .split("  dev)")
        .nth(1)
        .expect("the dev branch must exist")
        .split("  list)")
        .next()
        .expect("it ends where the next branch starts")
        .to_string()
}

/// `dev` reads the state **before** touching it.
///
/// what it owes you on the way out is the machine you walked in on, and after
/// its own `systemctl stop` that is no longer observable: a state read later
/// would say "stopped" about an instance that was serving a customer a second
/// earlier. Same rule as the installer's snapshot, for the same reason.
#[test]
fn dev_reads_the_state_before_stopping_and_arms_the_trap_first() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    let dev = dev_branch(&content);

    let read_at = dev
        .find("dev_state_on_entry=")
        .expect("dev must record the state it found");
    let armed_at = dev
        .find("trap restore_service")
        .expect("dev must arm the restore before it mutates anything");
    let stopped_at = dev
        .find("systemctl stop")
        .expect("dev stops the service: that has not changed");

    assert!(
        read_at < stopped_at,
        "the state is read before the stop, or it reads our own stop:\n{dev}"
    );
    assert!(
        armed_at < stopped_at,
        "the trap is armed before the stop: between those two lines a kill \
         would leave the service down with nobody remembering it was up:\n{dev}"
    );
}

/// every way out leads to the restore, not just the polite one.
///
/// the person this exists for is the one who closes the window and forgets —
/// and a closed window is `SIGHUP`, with nobody left to answer a prompt. A
/// restore that only ran on a normal exit would miss exactly the case that
/// motivated it.
#[test]
fn dev_restores_on_every_way_out_and_only_once() {
    let content = control_script_content("odoo18", "odoo", "odoo");

    assert!(
        content.contains("trap restore_service EXIT HUP INT TERM"),
        "the restore must be reached by a closed window and a kill too, not \
         only by a clean exit"
    );
    assert!(
        content.contains("trap - EXIT HUP INT TERM"),
        "and it must disarm itself: a signal trap and the EXIT trap would \
         otherwise both fire, asking the same question twice"
    );
}

/// the answer nobody types is the state that was found — in **both**
/// directions.
///
/// not a fixed "yes": with a service found stopped, a fixed yes means Enter
/// starts something somebody switched off on purpose, and Enter is precisely
/// what a distracted person presses. Deriving the default from the state found
/// keeps the choice free in both directions without giving up the project's
/// own invariant — *put back what you found*.
#[test]
fn the_silent_answer_is_the_state_that_was_found() {
    let content = control_script_content("odoo18", "odoo", "odoo");

    let restore = content
        .split("restore_service() {")
        .nth(1)
        .expect("the restore function must exist")
        .split("\ncase \"${1:-}\" in")
        .next()
        .expect("it ends where the verbs start");

    // found up: restoring is the silent answer.
    let up = restore
        .split(r#"if [ "${dev_state_on_entry:-}" = "active" ]; then"#)
        .nth(1)
        .expect("the branch for a service found active must exist")
        .split("else")
        .next()
        .expect("it ends at the other branch");
    assert!(
        up.contains("[Y/n]") && up.contains(r#"default="y""#),
        "found up, the default restarts it:\n{up}"
    );

    // found down: the offer stands, but Enter must not act on it.
    let down = restore
        .split("  else\n")
        .nth(1)
        .expect("the branch for a service found stopped must exist")
        .split("  fi")
        .next()
        .expect("it ends at the end of the if");
    assert!(
        down.contains("[y/N]") && down.contains(r#"default="n""#),
        "found stopped, the default leaves it alone:\n{down}"
    );
    assert!(
        down.contains("was already stopped when you entered dev"),
        "and it says why it is not restarting anything:\n{down}"
    );
}

/// the prompt is an **offer**, never the mechanism.
///
/// a question needs a terminal and somebody watching it. Neither is guaranteed
/// in the scenario this was written for, so the decision cannot depend on one:
/// with no terminal the rule is applied straight away, and with a terminal
/// nobody is watching it is applied when the wait runs out — otherwise the
/// question holds the service down for as long as it stays on screen.
#[test]
fn the_restore_decides_without_a_terminal_and_never_waits_forever() {
    let content = control_script_content("odoo18", "odoo", "odoo");

    assert!(
        content.contains("if [ -t 0 ]; then"),
        "the prompt happens only when there is somebody to answer it"
    );
    assert!(
        content.contains(r#"read -r -t "${DEV_ANSWER_TIMEOUT}""#),
        "and it is bounded: an unanswered question must fall back to the default"
    );
    assert!(
        content.contains("DEV_ANSWER_TIMEOUT="),
        "the wait is a named constant, not a number buried in the read"
    );
    assert!(
        content.contains(r#"  dev       stop it and open a shell"#)
            && content.contains("the way you found it"),
        "and the usage says what leaving dev now does"
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

/// runs the rendered helper's `dev` verb for real, against a fake machine.
///
/// the structural guards above read the SHAPE of the template; this one reads
/// the VALUE that comes out of it — the pairing this project asks for on every
/// guard, and here it is worth more than usual: what is being asserted is
/// **shell semantics** (a trap, `set -e`, a read with no terminal), which no
/// amount of looking at the text proves.
///
/// nothing of the host is touched: `sudo`, `systemctl` and `su` are scripts in
/// a temporary directory placed first on `PATH`, and the "machine state" is a
/// file. `su` stands in for the dev shell — `dev_shell` is what it runs, so a
/// test can act from *inside* the session.
///
/// returns `(final state, output, every systemctl invocation)`. The last one
/// matters: without a terminal no prompt is ever printed, so "it asked nothing
/// and did nothing" is only visible in what it *invoked*.
fn run_dev_against_a_fake_machine(
    initial_state: &str,
    dev_shell: &str,
) -> (String, String, Vec<String>) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    let state = dir.path().join("service-state");
    std::fs::write(&state, format!("{initial_state}\n")).expect("state");
    let calls = dir.path().join("systemctl-calls");
    std::fs::write(&calls, "").expect("calls");

    let write_exe = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, body).expect("write shim");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod shim");
    };

    // `is-active` answers with its OUTPUT and exits non-zero when the service
    // is down — the real one does exactly that, and the helper relies on it.
    write_exe(
        "systemctl",
        r#"#!/usr/bin/env bash
state_file="${FAKE_STATE}"
echo "$*" >> "${FAKE_CALLS}"
case "${1:-}" in
  is-active)
    s="$(cat "${state_file}")"
    echo "${s}"
    [ "${s}" = "active" ]
    ;;
  stop)  echo inactive > "${state_file}" ;;
  start) echo active   > "${state_file}" ;;
  *) ;;
esac
"#,
    );
    write_exe("sudo", "#!/usr/bin/env bash\nexec \"$@\"\n");
    // the dev shell itself: whatever the test wants to happen inside it.
    write_exe("su", &format!("#!/usr/bin/env bash\n{dev_shell}\n"));

    let script = dir.path().join("helper.sh");
    std::fs::write(&script, control_script_content("odoo18", "odoo", "odoo"))
        .expect("write helper");

    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("dev")
        .env("FAKE_STATE", &state)
        .env("FAKE_CALLS", &calls)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        // no terminal: this is the silent path, and it is the one the voice
        // exists for — a closed window has nobody to ask.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("bash must be available");

    let final_state = std::fs::read_to_string(&state)
        .expect("state")
        .trim()
        .to_string();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let calls = std::fs::read_to_string(&calls)
        .expect("calls")
        .lines()
        .map(|l| l.to_string())
        .collect();
    (final_state, text, calls)
}

/// found up, left up — without anybody answering anything.
#[test]
fn dev_puts_back_a_service_it_found_running() {
    let (state, out, calls) = run_dev_against_a_fake_machine("active", "exit 0");
    assert_eq!(
        state, "active",
        "an instance that was serving must be serving again when you leave:\n{out}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("start ")),
        "and it is this helper that put it back, not the fake state drifting: {calls:?}"
    );
}

/// found down, left down.
///
/// the other half of the same rule, and the one a fixed "yes" would have got
/// wrong: nobody answered, so nothing gets started that somebody had switched
/// off on purpose.
#[test]
fn dev_leaves_alone_a_service_it_found_stopped() {
    let (state, out, calls) = run_dev_against_a_fake_machine("inactive", "exit 0");
    assert_eq!(
        state, "inactive",
        "silence must not start what was already off:\n{out}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("start ")),
        "and it must not even try: {calls:?}"
    );
    assert!(
        out.contains("was already stopped when you entered dev"),
        "and it must say why it is not starting it:\n{out}"
    );
}

/// you started it yourself from inside: nothing to ask, nothing to do.
///
/// asserted on what was **invoked**, not on what was printed: with no terminal
/// the question is never printed anyway, so an output that stays quiet proves
/// nothing. A `start` issued at a service that is already up is the visible
/// half of the same mistake.
#[test]
fn dev_says_nothing_when_you_brought_the_service_back_yourself() {
    let (state, out, calls) = run_dev_against_a_fake_machine("active", "systemctl start odoo18");
    assert_eq!(state, "active");
    // the one the dev shell itself issued, and no other.
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("start ")).count(),
        1,
        "the state is already the one wanted: there is nothing left to do: {calls:?}\n{out}"
    );
    assert!(
        !out.contains("STOPPED") && !out.contains("Restart"),
        "and nothing left to say either:\n{out}"
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

/// `logs` follows **this** instance's journal, and takes an optional count.
///
/// read-only, so it is allowed to exist next to the verbs that mutate — but it
/// is still scoped to one unit: on a machine with two customers, a log that
/// mixed both would be worse than no log.
#[test]
fn logs_follows_this_instances_journal_only() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");
    let branch = content
        .split("  logs)")
        .nth(1)
        .expect("the logs branch must exist")
        .split("  status)")
        .next()
        .expect("it ends where the next branch starts");

    assert!(
        branch.contains(r#"journalctl -u "${SERVICE_NAME}""#),
        "the journal must be this instance's:\n{branch}"
    );
    assert!(branch.contains("-f"), "it follows: that is what it is for");
    assert!(
        branch.contains(r#""${2:-100}""#),
        "the number of lines is an optional argument, with a default"
    );
    assert!(
        content.contains("Ctrl-C stops reading; the service keeps running"),
        "the usage must say Ctrl-C does not stop Odoo — right after `dev`, which does stop it"
    );
}

/// `list` answers "what is on this machine" from **any** helper.
///
/// the discovery half of what a single shared controller would have given,
/// without the ownership it would have cost: every instance keeps a
/// self-sufficient tool — one that still works when another instance was
/// removed badly or its manifest is unreadable — and the listing tells you
/// which command drives what, so the action stays explicit.
#[test]
fn list_shows_the_machine_without_touching_it() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");
    let branch = content
        .split("  list)")
        .nth(1)
        .expect("the list branch must exist")
        .split("  logs)")
        .next()
        .expect("it ends where the next branch starts");

    assert!(
        branch.contains("list_instances"),
        "list is the listing, and the listing lives in one function:\n{branch}"
    );
    assert!(
        !branch.contains("systemctl start")
            && !branch.contains("systemctl stop")
            && !branch.contains("systemctl restart"),
        "a listing must not be able to act:\n{branch}"
    );
    assert!(
        content.contains("  list      "),
        "and the usage has to offer it, or nobody finds it"
    );
}
