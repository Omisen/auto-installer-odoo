//! [`CreateOdooUser`]: the decision logic through a mock, without root.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::error::StepError;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::create_odoo_user::{CreateOdooUser, CreateUserSnapshot};
use invok::system_ops::OwnerId;

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_home: PathBuf::from("/opt/odoo"),
        dry_run: false,
        ..Default::default()
    }
}

fn persisted(step: &CreateOdooUser) -> CreateUserSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

#[test]
fn created_by_us_runs_useradd_and_undo_userdel_without_r() {
    // no user and no home, to isolate the create/delete part.
    let cfg = MockConfig {
        user_exists: false,
        path_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert_eq!(persisted(&step).user_prestate, PreState::CreatedByUs);

    step.undo(&c).expect("undo");

    let ops = ops_of(&log);

    // run: `useradd` with the expected arguments.
    let created = ops.iter().find_map(|op| match op {
        Op::CreateUser(spec) => Some(spec),
        _ => None,
    });
    let spec = created.expect("useradd deve essere eseguito");
    assert_eq!(spec.name, "odoo");
    assert_eq!(spec.home, PathBuf::from("/opt/odoo"));
    assert!(spec.system && spec.create_home && spec.user_group);
    assert_eq!(spec.shell, "/bin/false");

    // run: an explicit chown and chmod on the home.
    assert!(ops.iter().any(
        |op| matches!(op, Op::ChownNamed { owner, group, .. } if owner == "odoo" && group == "odoo")
    ));
    assert!(ops
        .iter()
        .any(|op| matches!(op, Op::Chmod { mode, .. } if *mode == 0o750)));

    // undo: user and group removal, NEVER the home.
    assert!(ops.contains(&Op::DeleteUser("odoo".to_string())));
    assert!(ops.contains(&Op::DeleteGroup("odoo".to_string())));
    // the boundary's shape: the delete carries only the user name, no path —
    // the home belongs to another step's undo.
}

#[test]
fn preexisting_user_is_never_touched() {
    // an existing user: nothing created, nothing deleted.
    //
    // the home already belongs to them, which is the healthy situation and the
    // one where this step genuinely has nothing to do. a root-owned home would
    // trip A-V3-4's precondition, which is another test.
    let cfg = MockConfig {
        user_exists: true,
        path_exists: true,
        owner: OwnerId { uid: 999, gid: 999 },
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step).user_prestate, PreState::Preexisting);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // no mutation: a pre-existing user is never touched.
    assert!(
        ops.is_empty(),
        "un utente Preexisting non deve subire alcuna azione, trovato: {ops:?}"
    );
}

#[test]
fn undo_restores_original_owner_when_home_was_preexisting() {
    // a pre-existing home owned by someone else: after our chown the undo must
    // restore the original owner, not leave it to a deleted user.
    let original = OwnerId {
        uid: 1000,
        gid: 1000,
    };
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        owner: original,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step).home_original_owner, Some(original));

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::ChownNumeric {
            path: PathBuf::from("/opt/odoo"),
            id: original,
        }),
        "undo deve ripristinare l'owner originale della home, trovato: {ops:?}"
    );
    // and the deletion still carries no `-r`.
    assert!(ops.contains(&Op::DeleteUser("odoo".to_string())));
}

#[test]
fn dry_run_creates_nothing() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let mut c = ctx();
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // a dry run leaves the state untracked, so the undo is a no-op.
    assert_eq!(persisted(&step).user_prestate, PreState::Untracked);
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "dry-run non deve eseguire alcuna operazione"
    );
}

// --- A-V3-4: an existing user with an unusable home -------------------------

/// an existing user **and** a pre-existing root-owned home make the
/// installation impossible, and that must be said **before** mutating.
///
/// without this precondition the error arrived three steps later as a
/// *Permission denied* on a `mkdir`: a symptom naming neither the cause nor the
/// condition that makes it one.
#[test]
fn a_preexisting_user_with_a_root_owned_home_is_refused_before_mutating() {
    let cfg = MockConfig {
        user_exists: true,
        path_exists: true,
        owner: OwnerId { uid: 0, gid: 0 },
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    let err = step
        .snapshot(&c)
        .expect_err("una home di root con l'utente già esistente deve fermare l'installazione");

    let msg = err.to_string();
    assert!(
        msg.contains("/opt/odoo") && msg.contains("root"),
        "il messaggio deve nominare la home e il suo proprietario: {msg}"
    );
    assert!(
        msg.contains("chown") || msg.contains("rimuovila"),
        "il messaggio deve dire all'utente cosa può fare: {msg}"
    );

    // a precondition, not an undo: it fails having touched nothing.
    assert!(
        ops_of(&log).iter().all(|op| !matches!(
            op,
            Op::CreateUser(_) | Op::ChownNamed { .. } | Op::Chmod { .. }
        )),
        "nessuna mutazione prima del rifiuto: {:?}",
        ops_of(&log)
    );
}

/// the precondition concerns **only** a pre-existing user: when we create it, a
/// root-owned home is the norm — we just made it, and are about to hand it
/// over.
#[test]
fn a_root_owned_home_is_fine_when_we_create_the_user() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        owner: OwnerId { uid: 0, gid: 0 },
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c)
        .expect("con l'utente da creare, una home di root è lo stato normale");
    step.run(&c).expect("run");
}

// --- A-MD-3: a wanted outcome is not reported as a failure ------------------

/// **the defect, seen on every rollback of one family.**
///
/// ```text
/// WARN undo: groupdel fallito/orfano, proseguo (best-effort) group=odoo
///      error=comando `groupdel -- odoo` fallito (exit 6): group 'odoo' does not exist
/// ```
///
/// there `userdel` takes the primary group with it, so the following
/// `groupdel` finds nothing. the undo is correct — the group *is gone*, which
/// is the wanted result — but it was reported as a failure, so a successful
/// rollback looked suspicious.
///
/// A-V3-10's category: cosmetic, and insidious precisely because it appears
/// **every time** and teaches people to ignore warnings.
#[test]
fn a_group_removed_together_with_the_user_is_not_a_failure() {
    use invok::steps::create_odoo_user::group_already_gone;

    let gia_rimosso = StepError::CommandFailed {
        command: "groupdel -- odoo".to_string(),
        status: "6".to_string(),
        stderr: "groupdel: group 'odoo' does not exist\n".to_string(),
    };
    assert!(
        group_already_gone(&gia_rimosso),
        "exit 6 di groupdel significa «il gruppo non esiste», che qui è ciò che volevamo"
    );
}

/// but a **real** failure stays one: the group is still there, and that is a
/// leftover the user needs to know about.
#[test]
fn a_real_groupdel_failure_is_still_reported() {
    use invok::steps::create_odoo_user::group_already_gone;

    // the concrete case: the group is still another user's primary.
    let in_uso = StepError::CommandFailed {
        command: "groupdel -- odoo".to_string(),
        status: "8".to_string(),
        stderr: "groupdel: cannot remove the primary group of user 'altro'\n".to_string(),
    };
    assert!(
        !group_already_gone(&in_uso),
        "exit 8 è un ostacolo reale: il gruppo resta sul sistema"
    );

    for status in ["1", "2", "10", "spawn-failed", "signal"] {
        let altro = StepError::CommandFailed {
            command: "groupdel -- odoo".to_string(),
            status: status.to_string(),
            stderr: String::new(),
        };
        assert!(
            !group_already_gone(&altro),
            "'{status}' non è «il gruppo non esiste»: nel dubbio si avvisa"
        );
    }

    // an error that did not come from a command is not classifiable.
    assert!(!group_already_gone(&StepError::Precondition(
        "altro".into()
    )));
}

/// the discriminant is the **exit code**, not the text.
///
/// `groupdel` writes its message in the system's language, so a check on stderr
/// would fail on a localised machine — `apt-cache policy`'s trap. the code is
/// documented by shadow-utils and does not translate.
#[test]
fn the_verdict_does_not_depend_on_the_system_language() {
    use invok::steps::create_odoo_user::group_already_gone;

    let in_italiano = StepError::CommandFailed {
        command: "groupdel -- odoo".to_string(),
        status: "6".to_string(),
        stderr: "groupdel: il gruppo «odoo» non esiste\n".to_string(),
    };
    assert!(
        group_already_gone(&in_italiano),
        "il verdetto viene dal codice 6, non dal messaggio: su una macchina \
         localizzata il testo è un altro e la conclusione dev'essere la stessa"
    );
}
