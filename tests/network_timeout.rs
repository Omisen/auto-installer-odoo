//! the network-operation timeout (A3.1, closed in R2).
//!
//! two levels: the **primitive**, where a process that never ends is killed and
//! reported as a timeout, the pipes do not deadlock and a normal failure stays
//! a normal failure; and the **policy** — default, override, disabling.
//!
//! no test waits out a production timeout: the limits here are hundreds of
//! milliseconds, and the point is that the wait *ends* long before the command
//! would.

use std::time::{Duration, Instant};

use invok::error::StepError;
use invok::system_ops::{
    network_timeout, run_with_timeout, timeout_from_setting, DEFAULT_NETWORK_TIMEOUT_SECS,
    NETWORK_TIMEOUT_ENV,
};

// --- the primitive ----------------------------------------------------------

/// the case A3.1 described: a command that never returns must be killed on
/// expiry and produce a typed error rather than hanging.
#[test]
fn a_hanging_command_is_killed_and_reported_as_timeout() {
    let start = Instant::now();
    let err = run_with_timeout("sleep", &["60"], Duration::from_millis(200))
        .expect_err("a command that does not finish must time out");

    match err {
        StepError::Timeout { command, secs } => {
            assert!(command.contains("sleep"), "comando riportato: {command}");
            assert_eq!(secs, 0, "a sub-second limit rounds to 0s");
        }
        other => panic!("atteso Timeout, ottenuto: {other}"),
    }
    // the point of the whole fix: we do not wait out the command.
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the wait must end at the timeout, not at the command's end"
    );
}

/// a command finishing within the limit passes normally: the timeout must not
/// introduce failures where there were none.
#[test]
fn a_fast_command_succeeds_within_its_limit() {
    run_with_timeout("true", &[], Duration::from_secs(30)).expect("comando rapido");
}

/// a real failure stays `CommandFailed` with its stderr: the timeout branch
/// does not degrade the diagnostics.
#[test]
fn a_failing_command_is_still_command_failed_with_stderr() {
    let err = run_with_timeout(
        "sh",
        &["-c", "echo dettaglio-diagnostico >&2; exit 3"],
        Duration::from_secs(30),
    )
    .expect_err("exit 3 deve fallire");

    match err {
        StepError::CommandFailed { status, stderr, .. } => {
            assert_eq!(status, "3");
            assert!(
                stderr.contains("dettaglio-diagnostico"),
                "stderr perso: {stderr}"
            );
        }
        other => panic!("atteso CommandFailed, ottenuto: {other}"),
    }
}

/// a regression on pipe deadlock: a command writing **more** than the pipe
/// buffer must still complete. without the draining threads it would block, and
/// the timeout would disguise that as a slow network — a bug introduced by the
/// fix itself.
#[test]
fn a_verbose_command_does_not_deadlock_on_a_full_pipe() {
    let start = Instant::now();
    run_with_timeout(
        "sh",
        &["-c", "yes progress-line | head -n 60000 >&2"],
        Duration::from_secs(30),
    )
    .expect("a verbose command must not deadlock");
    assert!(start.elapsed() < Duration::from_secs(30));
}

// --- the policy -------------------------------------------------------------

#[test]
fn timeout_policy_default_override_and_disable() {
    // absent or non-numeric gives the documented default.
    assert_eq!(
        timeout_from_setting(None),
        Some(Duration::from_secs(DEFAULT_NETWORK_TIMEOUT_SECS))
    );
    assert_eq!(
        timeout_from_setting(Some("not-a-number")),
        Some(Duration::from_secs(DEFAULT_NETWORK_TIMEOUT_SECS))
    );
    // an explicit override, tolerating spaces.
    assert_eq!(
        timeout_from_setting(Some(" 42 ")),
        Some(Duration::from_secs(42))
    );
    // zero means no timeout: the escape hatch for a very slow line.
    assert_eq!(timeout_from_setting(Some("0")), None);
}

/// reads the environment but applies the same pure policy.
#[test]
fn network_timeout_reads_the_documented_env_var() {
    assert_eq!(
        network_timeout(),
        timeout_from_setting(std::env::var(NETWORK_TIMEOUT_ENV).ok().as_deref())
    );
}

/// the error message names the right variable: renaming the constant without
/// updating the text is caught here.
#[test]
fn timeout_error_message_names_the_env_var() {
    let err = StepError::Timeout {
        command: "sudo -u odoo -- git clone ...".to_string(),
        secs: DEFAULT_NETWORK_TIMEOUT_SECS,
    };
    let rendered = err.to_string();
    assert!(
        rendered.contains(NETWORK_TIMEOUT_ENV),
        "the message must say how to raise the limit: {rendered}"
    );
    assert!(rendered.contains("300"), "it must say how long it waited");
}
