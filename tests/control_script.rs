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
    // A-V6-20: the restore is an undo, and an undo carries on. `set -e` makes
    // the opposite the default, and a closed window makes every write fail.
    let restore = content
        .split("restore_service() {")
        .nth(1)
        .expect("the restore function must exist");
    assert!(
        restore
            .split("\ncase \"${1:-}\" in")
            .next()
            .expect("it ends where the verbs start")
            .contains("\n  set +e\n"),
        "the restore must be best-effort: under `set -e` its own first `echo` \
         killed it whenever the terminal was gone"
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
    run_dev_with_options(initial_state, dev_shell, None)
}

/// the other instances the fake machine carries: `(unit, the user it runs as)`,
/// answered the way `systemctl list-unit-files` and `systemctl show -p User`
/// answer. An empty user is systemd saying nothing, which the helper must treat
/// as "I do not know" rather than as root.
const FAKE_UNITS: &[(&str, &str)] = &[
    ("odoo18.service", "odoo"),
    ("odoo-cliente-x.service", "odoo-cliente-x"),
    // its user is deliberately NOT `odoo-altro`: the cascade can override
    // ODOO_USER, so anything that rebuilds the name instead of reading it gets
    // this one wrong.
    ("odoo-altro.service", "un-altro-utente"),
    // systemd has no answer for this one, which must mean "I do not know"
    // rather than "root".
    ("odoo-senza-utente.service", ""),
];

/// as above, but `broken_output_entry_state` runs the RESTORE on its own, with
/// stdout and stderr already closed, as the state of a terminal that went away
/// halfway through: every write from there fails, the way a write to a pty
/// whose master is gone fails with EIO.
///
/// the helper is **sourced** (`-h` prints the usage and returns), so what runs
/// is the real rendered function and not a copy of it. Breaking the output
/// from the very start would prove nothing: `dev` would die at its own first
/// `echo`, before stopping anything, and the machine would be untouched — the
/// dangerous window opens only once the service is already down.
fn run_dev_with_options(
    initial_state: &str,
    dev_shell: &str,
    broken_output_entry_state: Option<&str>,
) -> (String, String, Vec<String>) {
    let (state, out, calls, _) = run_helper(
        initial_state,
        dev_shell,
        broken_output_entry_state,
        &[],
        FAKE_UNITS,
    );
    (state, out, calls)
}

/// `dev <instance>` against the same fake machine.
///
/// returns `(state of THIS instance, output, systemctl invocations, `su`
/// invocations)`. The last one is the point: what has to be proved is *which
/// user* it became, and that comes from what `su` was called with.
fn run_dev_into(instance: &str) -> (String, String, Vec<String>, Vec<String>) {
    run_helper("active", "exit 0", None, &["dev", instance], FAKE_UNITS)
}

/// the same, on a machine laid out differently — for the cases the default
/// layout cannot express, like two unnamed installations.
fn run_dev_into_machine(
    instance: &str,
    units: &[(&str, &str)],
) -> (String, String, Vec<String>, Vec<String>) {
    run_helper("active", "exit 0", None, &["dev", instance], units)
}

fn run_helper(
    initial_state: &str,
    dev_shell: &str,
    broken_output_entry_state: Option<&str>,
    argv: &[&str],
    machine: &[(&str, &str)],
) -> (String, String, Vec<String>, Vec<String>) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    let state = dir.path().join("service-state");
    std::fs::write(&state, format!("{initial_state}\n")).expect("state");
    let calls = dir.path().join("systemctl-calls");
    std::fs::write(&calls, "").expect("calls");
    let su_calls = dir.path().join("su-calls");
    std::fs::write(&su_calls, "").expect("su calls");
    // the machine's other instances, as a table the shim reads.
    let units = dir.path().join("units");
    std::fs::write(
        &units,
        machine
            .iter()
            .map(|(u, who)| format!("{u} {who}\n"))
            .collect::<String>(),
    )
    .expect("units");

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
    # only this helper's own service has a state file; the others are up.
    if [ "${2:-}" = "odoo18" ] || [ "${2:-}" = "odoo18.service" ]; then
      s="$(cat "${state_file}")"
    else
      s=active
    fi
    echo "${s}"
    [ "${s}" = "active" ]
    ;;
  stop)  echo inactive > "${state_file}" ;;
  start) echo active   > "${state_file}" ;;
  list-unit-files)
    # the real one prints "<unit> <state>"; the helper reads the first field.
    while read -r u _who; do
      [ -n "${u}" ] && echo "${u} enabled"
    done < "${FAKE_UNITS}"
    ;;
  show)
    # `show -p User --value <unit>`: prints nothing when systemd has no answer.
    unit="${!#}"
    while read -r u who; do
      if [ "${u}" = "${unit}" ] || [ "${u}" = "${unit}.service" ]; then
        echo "${who}"
      fi
    done < "${FAKE_UNITS}"
    ;;
  *) ;;
esac
"#,
    );
    write_exe("sudo", "#!/usr/bin/env bash\nexec \"$@\"\n");
    // the real one follows the journal and never returns; here it records what
    // it was asked for and gets out of the way. Same log as systemctl, with a
    // prefix, so the assertions that look for `start `/`stop ` are unaffected.
    write_exe(
        "journalctl",
        "#!/usr/bin/env bash\necho \"journalctl $*\" >> \"${FAKE_CALLS}\"\n",
    );
    // the dev shell itself: whatever the test wants to happen inside it. It
    // records its own argv, which is how "who did it become" is observed.
    write_exe(
        "su",
        &format!("#!/usr/bin/env bash\necho \"$*\" >> \"${{FAKE_SU_CALLS}}\"\n{dev_shell}\n"),
    );

    let script = dir.path().join("helper.sh");
    std::fs::write(&script, control_script_content("odoo18", "odoo", "odoo"))
        .expect("write helper");

    let mut cmd = std::process::Command::new("bash");
    match broken_output_entry_state {
        Some(entry) => {
            cmd.arg("-c")
                .arg(format!(
                    r#"source "$0" -h >/dev/null 2>&1; dev_state_on_entry={entry}; exec 1>&- 2>&-; restore_service"#
                ))
                .arg(&script);
        }
        None => {
            cmd.arg(&script);
            if argv.is_empty() {
                cmd.arg("dev");
            } else {
                cmd.args(argv);
            }
        }
    }
    let out = cmd
        .env("FAKE_STATE", &state)
        .env("FAKE_CALLS", &calls)
        .env("FAKE_SU_CALLS", &su_calls)
        .env("FAKE_UNITS", &units)
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
    let read_lines = |p: &std::path::Path| -> Vec<String> {
        std::fs::read_to_string(p)
            .expect("call log")
            .lines()
            .map(|l| l.to_string())
            .collect()
    };
    (final_state, text, read_lines(&calls), read_lines(&su_calls))
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

/// the restore must not be killed by its own output.
///
/// found on the VM, and it had defeated the whole feature (`A-V6-20`): with the
/// window closed the pty master is gone, so the first `echo` of the restore
/// gets EIO — and under `set -e` that ended the shell *before* it reached the
/// service. The customer's Odoo stayed down, in precisely the scenario the
/// behaviour was written for, with nobody watching and nothing printed.
///
/// this reproduces the CLASS rather than the pty: with stdout and stderr closed
/// every write fails with EBADF, and the question is the same one — does the
/// action survive the telling of it? A mock could not have found this, but once
/// found it can be held.
#[test]
fn the_restore_survives_an_output_that_fails() {
    // the service is down (dev stopped it) and it was up on the way in.
    let (state, _out, calls) = run_dev_with_options("inactive", "exit 0", Some("active"));
    assert_eq!(
        state, "active",
        "a write that fails must not stop the restore: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("start ")),
        "and it must have got all the way to the service: {calls:?}"
    );
}

/// and the same when there is nothing to put back: a failing write must not
/// turn "leave it alone" into an action either.
#[test]
fn an_output_that_fails_does_not_start_what_was_already_stopped() {
    let (state, _out, calls) = run_dev_with_options("inactive", "exit 0", Some("inactive"));
    assert_eq!(state, "inactive");
    assert!(
        !calls.iter().any(|c| c.starts_with("start ")),
        "best-effort must not mean careless: {calls:?}"
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

/// `dev <instance>` becomes that instance's user — and touches nothing else.
///
/// `A-V6-18`. The need was reaching another instance's files as whoever owns
/// them; the answer is `sudo`, not a loosened permission, because its home is
/// `0750` and its config `0640` on purpose — the password and the attachments
/// are in there.
///
/// what this test guards is the line the feature must not cross: it opens a
/// shell, it does not stop anybody's service. A helper able to stop another
/// instance is the hazard one-helper-per-instance exists to prevent, and an
/// argument would have let it in through the front door.
#[test]
fn dev_with_an_instance_opens_a_shell_and_stops_nothing() {
    let (state, out, systemctl, su) = run_dev_into("cliente-x");

    assert_eq!(
        su.len(),
        1,
        "it must enter exactly one shell: {su:?}\n{out}"
    );
    assert!(
        su[0].contains("odoo-cliente-x"),
        "and as that instance's user: {su:?}\n{out}"
    );
    assert!(
        !systemctl
            .iter()
            .any(|c| c.starts_with("stop ") || c.starts_with("start ")),
        "nothing is stopped or started, not even this helper's own service: {systemctl:?}"
    );
    assert_eq!(
        state, "active",
        "and the helper's own service is untouched too"
    );
    assert!(
        out.contains("its service is left alone"),
        "it has to say that it did not stop anything:\n{out}"
    );
}

/// the same instance, named the three ways `list` makes plausible.
///
/// whoever just read `list` sees `odoo-cliente-x.service`; whoever read the
/// README types `cliente-x`. Making them translate would be a papercut in the
/// one command whose whole purpose is convenience.
#[test]
fn an_instance_can_be_named_the_way_list_prints_it() {
    for name in ["cliente-x", "odoo-cliente-x", "odoo-cliente-x.service"] {
        let (_, out, _, su) = run_dev_into(name);
        assert!(
            su.len() == 1 && su[0].contains("odoo-cliente-x"),
            "'{name}' must reach the same instance: {su:?}\n{out}"
        );
    }
}

/// `default` is the historical instance, whose unit carries the version.
///
/// the same reserved word `rollback --instance` and `list` already use: an
/// instance cannot be called `default`, so it can safely mean "the unnamed
/// one" — and the unnamed one's unit is `odoo18`, a version rather than an
/// identity, which is exactly why it needs a name that is not its unit's.
#[test]
fn default_names_the_historical_instance() {
    let (_, out, _, su) = run_dev_into("default");
    assert!(
        su.len() == 1 && su[0].contains("odoo") && !su[0].contains("cliente-x"),
        "'default' must reach the unprefixed unit's user: {su:?}\n{out}"
    );
}

/// two unnamed installations make `default` ambiguous, and ambiguity is
/// **refused**, not settled.
///
/// the unnamed instance's unit carries the Odoo **version**, so a machine can
/// carry `odoo17` and `odoo18` at once — a migration in progress is exactly
/// that. Picking one by a precedence rule would decide *which user you become*
/// on a coin toss, and this project settles that class of question by making it
/// impossible rather than clever: the same reason `rollback` without arguments
/// refuses and lists.
#[test]
fn two_unnamed_installations_make_default_ambiguous_and_it_is_refused() {
    let machine: &[(&str, &str)] = &[("odoo17.service", "odoo"), ("odoo18.service", "odoo")];
    let (_, out, _, su) = run_dev_into_machine("default", machine);

    assert!(su.is_empty(), "it must not pick one of them: {su:?}\n{out}");
    assert!(
        out.contains("matches more than one instance"),
        "and it must say the name was ambiguous:\n{out}"
    );
    assert!(
        out.contains("Odoo services on this machine:"),
        "listing them, so the exact name is one copy away:\n{out}"
    );
}

/// and naming one of the two exactly does work — the refusal above is about
/// ambiguity, not about the unnamed instance being unreachable.
#[test]
fn naming_an_unnamed_installation_exactly_still_works() {
    let machine: &[(&str, &str)] = &[("odoo17.service", "vecchio"), ("odoo18.service", "nuovo")];
    let (_, out, _, su) = run_dev_into_machine("odoo17", machine);
    assert!(
        su.len() == 1 && su[0].contains("vecchio"),
        "'odoo17' names one of them without ambiguity: {su:?}\n{out}"
    );
}

/// the user is READ from systemd, never rebuilt from the instance name.
///
/// `odoo-<name>` is only the DEFAULT the installer derives: the CLI/.env
/// cascade can override `ODOO_USER`, so the name is a guess and the unit is the
/// record. It is the project's standing rule about ownership — it gets reread,
/// not re-derived — and the fake machine is built to punish the other choice:
/// `odoo-altro` runs as `un-altro-utente`.
#[test]
fn the_user_is_read_from_the_unit_not_rebuilt_from_the_name() {
    let (_, out, systemctl, su) = run_dev_into("altro");
    assert!(
        su.len() == 1 && su[0].contains("un-altro-utente"),
        "rebuilding `odoo-<name>` would have become the wrong user: {su:?}\n{out}"
    );
    assert!(
        systemctl.iter().any(|c| c.starts_with("show ")),
        "and the answer must come from systemd: {systemctl:?}"
    );
}

/// an instance nobody has is an error that shows what there is.
#[test]
fn an_unknown_instance_fails_and_shows_what_exists() {
    let (_, out, _, su) = run_dev_into("non-esiste");
    assert!(su.is_empty(), "it must not enter anything: {su:?}");
    assert!(
        out.contains("no Odoo instance on this machine answers to 'non-esiste'"),
        "and it must say so:\n{out}"
    );
    assert!(
        out.contains("Odoo services on this machine:"),
        "showing what is actually here, rather than leaving you to guess:\n{out}"
    );
}

/// when systemd will not say who a unit runs as, we do not enter it.
///
/// an empty `User=` means "no answer", and the tempting fallback — root — turns
/// a convenience into a privilege surprise. Fail closed, like every other
/// unreadable answer in this project.
#[test]
fn a_unit_whose_user_is_unknown_is_refused() {
    let (_, out, systemctl, su) = run_dev_into("senza-utente");
    assert!(
        su.is_empty(),
        "with no answer it must not become anybody — least of all root: {su:?}\n{out}"
    );
    assert!(
        out.contains("cannot tell which user"),
        "and it must say why it stopped:\n{out}"
    );
    assert!(
        !systemctl
            .iter()
            .any(|c| c.starts_with("stop ") || c.starts_with("start ")),
        "and it must not have touched anything on the way: {systemctl:?}"
    );
}

/// the instance argument goes to the verbs that LOOK, and to no other.
///
/// `A-V6-17`, ratified as its useful half. A single shared helper driving every
/// instance was the proposal; what was taken is the part that costs nothing —
/// `status`, `logs` and `dev` can name another instance, because looking at
/// somebody else's instance is not the hazard. Starting it is.
///
/// so this is the line, and it is the same one the header has always declared:
/// a mutating `systemctl` names `${SERVICE_NAME}` and nothing else. The
/// resolver's answer must never reach one.
#[test]
fn only_the_read_only_verbs_take_an_instance() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");

    for (verb, next) in [
        ("  start)", "  stop)"),
        ("  stop)", "  restart)"),
        ("  restart)", "  dev)"),
    ] {
        let branch = content
            .split(verb)
            .nth(1)
            .unwrap_or_else(|| panic!("the {verb} branch must exist"))
            .split(next)
            .next()
            .expect("it ends where the next branch starts");
        assert!(
            !branch.contains("resolve_instance_unit") && !branch.contains("RESOLVED_UNIT"),
            "a mutating verb must not be able to name another instance:\n{branch}"
        );
        assert!(
            !branch.contains("${2"),
            "and it must not read an argument at all — silence is the guard:\n{branch}"
        );
    }

    // the resolver's answer is only ever read, never mutated through.
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.contains("RESOLVED_UNIT") {
            assert!(
                !line.contains("systemctl start")
                    && !line.contains("systemctl stop")
                    && !line.contains("systemctl restart"),
                "the resolved unit must never reach a mutating verb: {line}"
            );
        }
    }
}

/// one resolver, not three.
///
/// `dev`, `status` and `logs` must agree on what a name means and on how a bad
/// one is refused. Three copies would be three chances to drift — the shape of
/// duplication this project has paid for repeatedly, and the reason `A-V6-17`
/// was cut down instead of taken whole.
#[test]
fn the_instance_resolver_exists_once() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    assert_eq!(
        content.matches("resolve_instance_unit() {").count(),
        1,
        "the resolution lives in one function"
    );
    assert_eq!(
        content.matches("systemctl list-unit-files").count(),
        2,
        "and it is asked of systemd in two places only: the listing, and the resolver"
    );
    for caller in ["dev_into_another_instance", "logs_unit=", "status_unit="] {
        assert!(
            content.contains(caller),
            "'{caller}' must be there to use it"
        );
    }
}

/// `status <instance>` reports on that one, and still lists the machine.
#[test]
fn status_can_report_on_another_instance() {
    let content = control_script_content("odoo18", "odoo", "odoo");
    let branch = content
        .split("  status)")
        .nth(1)
        .expect("the status branch must exist")
        .split("  -h|--help|help)")
        .next()
        .expect("it ends where the next branch starts");

    assert!(
        branch.contains(r#"status_unit="${SERVICE_NAME}""#),
        "with no name it is still this instance:\n{branch}"
    );
    assert!(
        branch.contains(r#"systemctl status "${status_unit}""#),
        "and the named one otherwise:\n{branch}"
    );
    assert!(
        branch.contains("list_instances"),
        "the listing stays: that is what makes the next command obvious:\n{branch}"
    );
}

/// `logs <instance>` follows the other one's journal, and `logs 500` still
/// means five hundred lines of ours.
#[test]
fn logs_tells_an_instance_from_a_line_count() {
    let journal = |argv: &[&str]| -> String {
        let (_, out, calls, _) = run_helper("active", "exit 0", None, argv, FAKE_UNITS);
        calls
            .iter()
            .find(|c| c.starts_with("journalctl "))
            .unwrap_or_else(|| panic!("no journal was opened for {argv:?}:\n{out}\n{calls:?}"))
            .clone()
    };

    // a name: the other instance's journal, and the count moves along one.
    let named = journal(&["logs", "cliente-x", "5"]);
    assert!(
        named.contains("-u odoo-cliente-x.service") && named.contains("-n 5"),
        "'logs cliente-x 5' must follow that instance, 5 lines: {named}"
    );

    // a number: still five hundred lines of OUR log. This is the compatibility
    // that the digits rule exists to keep.
    let counted = journal(&["logs", "500"]);
    assert!(
        counted.contains("-u odoo18") && counted.contains("-n 500"),
        "'logs 500' must stay 500 lines of this instance: {counted}"
    );

    // and nothing: ours, with the historical default.
    let bare = journal(&["logs"]);
    assert!(
        bare.contains("-u odoo18") && bare.contains("-n 100"),
        "'logs' alone must not have changed at all: {bare}"
    );
}

/// `status <instance>` really asks about **that** unit.
///
/// the structural guard above reads the shape — `systemctl status
/// "${status_unit}"` — and a shape is not a value: assigning the wrong thing to
/// `status_unit` leaves the shape intact. A mutation proved exactly that, so
/// the value gets its own guard.
#[test]
fn status_with_an_instance_asks_about_that_unit() {
    let (_, out, calls, _) = run_helper(
        "active",
        "exit 0",
        None,
        &["status", "cliente-x"],
        FAKE_UNITS,
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("status odoo-cliente-x.service")),
        "it must ask systemd about the named instance: {calls:?}\n{out}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("status odoo18")),
        "and not about its own: {calls:?}"
    );

    // and with no name, nothing changed.
    let (_, _, calls, _) = run_helper("active", "exit 0", None, &["status"], FAKE_UNITS);
    assert!(
        calls.iter().any(|c| c.starts_with("status odoo18")),
        "bare `status` is still this instance: {calls:?}"
    );
}

/// a bad name is refused the same way from every verb that takes one.
///
/// the point of a single resolver: `dev`, `logs` and `status` cannot disagree
/// about what does not exist.
#[test]
fn every_verb_that_takes_an_instance_refuses_a_bad_one_alike() {
    for verb in ["dev", "logs", "status"] {
        let (_, out, calls, su) =
            run_helper("active", "exit 0", None, &[verb, "non-esiste"], FAKE_UNITS);
        assert!(
            out.contains("no Odoo instance on this machine answers to 'non-esiste'"),
            "'{verb} non-esiste' must refuse in the same words:\n{out}"
        );
        assert!(
            su.is_empty() && !calls.iter().any(|c| c.starts_with("journalctl ")),
            "'{verb}' must not act on anything after refusing: {calls:?} {su:?}"
        );
    }
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

/// `logs` follows **one** journal: this instance's, or the one it was told.
///
/// read-only, which is the whole reason an instance is allowed here at all
/// (`A-V6-17`) — but it is still scoped to a single unit: on a machine with two
/// customers, a log that mixed both would be worse than no log.
#[test]
fn logs_follows_one_instances_journal_only() {
    let content = control_script_content("odoo-cliente-x", "odoo-cliente-x", "odoo-cliente-x");
    let branch = content
        .split("  logs)")
        .nth(1)
        .expect("the logs branch must exist")
        .split("  status)")
        .next()
        .expect("it ends where the next branch starts");

    assert!(
        branch.contains(r#"logs_unit="${SERVICE_NAME}""#),
        "with no name it is still this instance's journal:\n{branch}"
    );
    assert!(
        branch.contains(r#"journalctl -u "${logs_unit}""#),
        "and exactly one unit is followed, never a glob:\n{branch}"
    );
    assert!(branch.contains("-f"), "it follows: that is what it is for");
    assert!(
        branch.contains(r#"logs_lines="${2:-100}""#) && branch.contains(r#""${3:-100}""#),
        "the count stays optional, and moves along when a name is given:\n{branch}"
    );
    // `logs 500` meant "500 lines" long before an instance could be named, and
    // it has to keep meaning that. The discriminator is not a convention
    // invented here: an instance name must begin with a letter, so a run of
    // digits can never be one.
    assert!(
        branch.contains("is_line_count"),
        "a number must still be read as a count, not looked up as an instance:\n{branch}"
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
