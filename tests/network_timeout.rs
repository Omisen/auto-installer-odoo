//! Test del timeout sulle operazioni di rete (audit A3.1, chiuso in R2).
//!
//! Due livelli:
//! - la **primitiva** (`run_with_timeout`): un processo che non termina viene
//!   ucciso e riportato come `StepError::Timeout`, i pipe non vanno in
//!   deadlock, un fallimento normale resta `CommandFailed`;
//! - la **politica** (`timeout_from_setting`): default, override, disattivazione.
//!
//! Nessun test attende davvero il timeout di produzione: i limiti qui sono di
//! centinaia di millisecondi, e il punto è proprio che l'attesa *finisca* molto
//! prima della durata del comando.

use std::time::{Duration, Instant};

use odoo_installer::error::StepError;
use odoo_installer::system_ops::{
    network_timeout, run_with_timeout, timeout_from_setting, DEFAULT_NETWORK_TIMEOUT_SECS,
    NETWORK_TIMEOUT_ENV,
};

// --- La primitiva ------------------------------------------------------------

/// Il caso che A3.1 descriveva: un comando che non ritorna mai. Deve essere
/// ucciso alla scadenza e produrre un errore tipizzato, non appendere.
#[test]
fn a_hanging_command_is_killed_and_reported_as_timeout() {
    let start = Instant::now();
    let err = run_with_timeout("sleep", &["60"], Duration::from_millis(200))
        .expect_err("un comando che non termina deve scadere");

    match err {
        StepError::Timeout { command, secs } => {
            assert!(command.contains("sleep"), "comando riportato: {command}");
            assert_eq!(secs, 0, "il limite sub-secondo si arrotonda a 0s");
        }
        other => panic!("atteso Timeout, ottenuto: {other}"),
    }
    // Il punto dell'intero fix: non si aspettano i 60 secondi del comando.
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "l'attesa deve terminare al timeout, non alla fine del comando"
    );
}

/// Un comando che termina entro il limite passa normalmente: il timeout non
/// deve introdurre fallimenti dove prima non ce n'erano.
#[test]
fn a_fast_command_succeeds_within_its_limit() {
    run_with_timeout("true", &[], Duration::from_secs(30)).expect("comando rapido");
}

/// Un fallimento vero (exit code) resta `CommandFailed` con lo stderr
/// catturato: il ramo con timeout non degrada la diagnostica.
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

/// Regressione sul deadlock dei pipe: un comando che scrive **più** del buffer
/// del pipe (64 KB) su stderr deve comunque completare. Senza i thread che
/// drenano stdout/stderr si bloccherebbe, e il timeout lo maschererebbe da
/// "rete lenta" — un bug introdotto dal fix stesso.
#[test]
fn a_verbose_command_does_not_deadlock_on_a_full_pipe() {
    let start = Instant::now();
    run_with_timeout(
        "sh",
        &["-c", "yes riga-di-progresso | head -n 60000 >&2"],
        Duration::from_secs(30),
    )
    .expect("un comando verboso non deve andare in deadlock");
    assert!(start.elapsed() < Duration::from_secs(30));
}

// --- La politica -------------------------------------------------------------

#[test]
fn timeout_policy_default_override_and_disable() {
    // Assente o non numerico → default documentato.
    assert_eq!(
        timeout_from_setting(None),
        Some(Duration::from_secs(DEFAULT_NETWORK_TIMEOUT_SECS))
    );
    assert_eq!(
        timeout_from_setting(Some("non-un-numero")),
        Some(Duration::from_secs(DEFAULT_NETWORK_TIMEOUT_SECS))
    );
    // Override esplicito (spazi tollerati).
    assert_eq!(
        timeout_from_setting(Some(" 42 ")),
        Some(Duration::from_secs(42))
    );
    // Zero = nessun timeout: l'escape hatch per chi ha una linea lentissima.
    assert_eq!(timeout_from_setting(Some("0")), None);
}

/// `network_timeout()` legge l'ambiente ma applica la stessa politica pura.
#[test]
fn network_timeout_reads_the_documented_env_var() {
    assert_eq!(
        network_timeout(),
        timeout_from_setting(std::env::var(NETWORK_TIMEOUT_ENV).ok().as_deref())
    );
}

/// Il messaggio d'errore cita la variabile giusta: se qualcuno rinomina la
/// costante senza aggiornare il testo, questo test se ne accorge.
#[test]
fn timeout_error_message_names_the_env_var() {
    let err = StepError::Timeout {
        command: "sudo -u odoo -- git clone ...".to_string(),
        secs: DEFAULT_NETWORK_TIMEOUT_SECS,
    };
    let rendered = err.to_string();
    assert!(
        rendered.contains(NETWORK_TIMEOUT_ENV),
        "il messaggio deve dire come alzare il limite: {rendered}"
    );
    assert!(rendered.contains("300"), "deve dire quanto ha atteso");
}
