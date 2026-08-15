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
    serde_json::from_value(step.snapshot_value()).expect("the snapshot must serialise")
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
    let spec = created.expect("useradd must run");
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
        "a Preexisting user must undergo no action, found: {ops:?}"
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
        "the undo must restore the home's original owner, found: {ops:?}"
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
        "a dry run must perform no operation"
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
        .expect_err("a root-owned home with the user already there must stop the installation");

    let msg = err.to_string();
    assert!(
        msg.contains("/opt/odoo") && msg.contains("root"),
        "the message must name the home and its owner: {msg}"
    );
    assert!(
        msg.contains("chown") || msg.contains("remove it"),
        "the message must tell the user what they can do: {msg}"
    );

    // a precondition, not an undo: it fails having touched nothing.
    assert!(
        ops_of(&log).iter().all(|op| !matches!(
            op,
            Op::CreateUser(_) | Op::ChownNamed { .. } | Op::Chmod { .. }
        )),
        "no mutation before the refusal: {:?}",
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
        .expect("with the user still to create, a root-owned home is the normal state");
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
        "groupdel's exit 6 means \"the group does not exist\", which here is what we wanted"
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
        stderr: "groupdel: cannot remove the primary group of user 'other'\n".to_string(),
    };
    assert!(
        !group_already_gone(&in_uso),
        "exit 8 is a real obstacle: the group stays on the system"
    );

    for status in ["1", "2", "10", "spawn-failed", "signal"] {
        let other = StepError::CommandFailed {
            command: "groupdel -- odoo".to_string(),
            status: status.to_string(),
            stderr: String::new(),
        };
        assert!(
            !group_already_gone(&other),
            "'{status}' is not \"the group does not exist\": in doubt we warn"
        );
    }

    // an error that did not come from a command is not classifiable.
    assert!(!group_already_gone(&StepError::Precondition(
        "other".into()
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
        stderr: "groupdel: group 'odoo' does not exist\n".to_string(),
    };
    assert!(
        group_already_gone(&in_italiano),
        "the verdict comes from the exit code, not from the message: on a localised \
         machine the text differs and the conclusion must be the same"
    );
}

/// `A-V6-12`: the home's **mode** is restored like its owner.
///
/// `run` sets `0750` on a home that may be somebody else's with permissions of
/// their choosing, and until now only the owner came back. handing a directory
/// to its owner with permissions we picked is not handing it back — the same
/// asymmetry R11 found on the nginx default site, and the same one `A-V6-9`
/// fixes one level up.
///
/// found by the model, not by reading: once it started answering `mode_of` with
/// what had actually been set, the "and that state is the virgin system"
/// assertion in `tests/rehydrate.rs` stopped holding.
#[test]
fn a_preexisting_homes_mode_is_restored_like_its_owner() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        owner: OwnerId {
            uid: 1000,
            gid: 1000,
        },
        dir_mode: 0o700,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        persisted(&step).home_original_mode,
        Some(0o700),
        "what the undo has to put back must be read before the run changes it"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    let home = PathBuf::from("/opt/odoo");
    let chmods: Vec<u32> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Chmod { path, mode } if path == &home => Some(*mode),
            _ => None,
        })
        .collect();
    assert_eq!(
        chmods,
        vec![0o750, 0o700],
        "the run takes the home to 0750, the undo gives it back exactly as it was"
    );
}

/// a home that did not exist has no mode of its own to restore: it belongs to
/// `prepare-opt-root`, whose undo removes it.
#[test]
fn a_home_we_found_absent_has_no_mode_to_restore() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step).home_original_mode, None);
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::Chmod { mode, .. } if *mode != 0o750)),
        "no restoring chmod when there was nothing of anybody else's to restore"
    );
}

/// `A-V3-24`: `useradd` succeeded and the `chmod` after it did not — the user
/// **exists**, and the undo has to remove it.
///
/// the mutation that revealed this test was missing: moving the promotion back
/// after the `chmod` left the whole suite green while a failed installation
/// left a system user on the machine for good. the rule the fix encodes is
/// *ours from the moment it exists, not once it is tidy*.
#[test]
fn a_user_created_before_a_later_failure_is_still_undone() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        chmod_fails: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect_err("the run must fail on the chmod");
    assert_eq!(
        persisted(&step).user_prestate,
        PreState::CreatedByUs,
        "the user is on the machine: the manifest has to say it is ours"
    );

    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::DeleteUser(u) if u == "odoo")),
        "a system user left behind by a failed installation is exactly what must not happen"
    );
}
