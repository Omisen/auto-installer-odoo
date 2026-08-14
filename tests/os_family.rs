//! M0: the distribution family is **re-read**, never re-derived.
//!
//! with two package managers the delta's undo has to know which to invoke, and
//! the only acceptable source is the manifest written by the installation that
//! created those artifacts.
//!
//! there is one delicate point — `InstallConfig::to_context` — and a test
//! written to die if that line disappears, because the defect would otherwise
//! be **silent**: the family would fall back to the default and the field would
//! only see a package manager failing on a machine that does not have it.

use std::io::Write;
use std::path::{Path, PathBuf};

use invok::checks::{
    check_os_from, is_newer_than_tested, os_id_from, required_commands, validate_os, CheckError,
    OsInfo,
};
use invok::context::Context;
use invok::distro::{family_mismatch, OsFamily};
use invok::state::{
    start_decision, InstallConfig, InstallState, PreState, StartDecision, StepRecord,
};

// --- helpers ----------------------------------------------------------------

fn write_os_release(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("os-release");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

fn config_for(family: OsFamily) -> InstallConfig {
    InstallConfig {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        odoo_home: PathBuf::from("/opt/odoo"),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        port: 8069,
        odoo_logfile: None,
        with_nginx: false,
        sudo_user: None,
        os_family: family,
        installer_version: None,
    }
}

// --- derivation: one gate, and no fallback ----------------------------------

#[test]
fn the_family_comes_from_the_os_id_and_nowhere_else() {
    assert_eq!(OsFamily::from_os_id("ubuntu"), Some(OsFamily::Debian));
    assert_eq!(OsFamily::from_os_id("debian"), Some(OsFamily::Debian));
    assert_eq!(OsFamily::from_os_id("fedora"), Some(OsFamily::Fedora));

    // a distribution we do not handle has no family: `None`, not a fallback.
    // the only place that decision is taken, and it must be able to say no.
    assert_eq!(OsFamily::from_os_id("arch"), None);
    assert_eq!(OsFamily::from_os_id(""), None);
}

/// `ID_LIKE` does **not** open the door to derivatives.
///
/// several declare it, and honouring it would let them in without anyone ever
/// having tried them. for a new family we start closed — no contradiction with
/// A5.1-bis, which is about not rejecting *newer* releases of a supported
/// family.
#[test]
fn id_like_does_not_admit_untested_derivatives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rocky = write_os_release(
        dir.path(),
        "ID=rocky\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.3\"\n",
    );
    assert!(
        matches!(check_os_from(&rocky), Err(CheckError::UnsupportedOs { .. })),
        "a derivative declaring ID_LIKE=fedora must not slip in through the window"
    );
}

#[test]
fn a_supported_os_carries_its_family() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ubuntu = write_os_release(
        dir.path(),
        "ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n",
    );
    assert_eq!(
        check_os_from(&ubuntu).expect("ubuntu ok").family,
        OsFamily::Debian
    );

    let debian = write_os_release(
        dir.path(),
        "ID=debian\nVERSION_ID=\"12\"\nVERSION_CODENAME=bookworm\n",
    );
    assert_eq!(
        check_os_from(&debian).expect("debian ok").family,
        OsFamily::Debian
    );
}

/// the rpm family is **accepted**, with its own version threshold.
///
/// in M0 this same case was a refusal: the family was recognised but had no
/// backend, and accepting it would have produced an installation that stops
/// halfway. with the backend the answer changes — a **deliberate** change.
#[test]
fn fedora_is_accepted_from_its_minimum_version() {
    let fedora = |version: &str| OsInfo {
        id: "fedora".to_string(),
        version: version.to_string(),
        codename: None,
        family: OsFamily::Fedora,
    };

    assert!(
        validate_os(&fedora("40")).is_ok(),
        "40 is the minimum threshold"
    );
    assert!(validate_os(&fedora("41")).is_ok());

    let err = validate_os(&fedora("39")).expect_err("below the threshold it is refused");
    assert!(
        matches!(err, CheckError::UnsupportedVersion { .. }),
        "UnsupportedVersion expected, found {err:?}"
    );
}

/// the threshold is open upwards here too, but "we accept" does not mean "we
/// keep quiet".
///
/// the CI now runs the full cycle on two releases of this family, by two
/// different routes — one where the system Python is covered by Odoo's pins,
/// one where the venv is built on an interpreter installed for the occasion. so
/// the warning is silent on both, correctly: what differs there is **handled**,
/// not ignored. an uncovered system Python is reported by its own warning, with
/// its own constant.
#[test]
fn only_a_fedora_newer_than_the_ci_one_is_flagged() {
    assert!(
        !is_newer_than_tested("fedora", "41"),
        "the CI really installs on Fedora 41: warning here would be a false alarm"
    );
    assert!(
        !is_newer_than_tested("fedora", "44"),
        "since M11 the CI really installs on Fedora 44 too: the warning would be false"
    );
    assert!(
        !is_newer_than_tested("fedora", "40"),
        "40 is the minimum threshold and is no newer than the tested one"
    );
    assert!(
        is_newer_than_tested("fedora", "45"),
        "a release past the tested one must be flagged: that is the information needed \
         when the package names or the checksum pin do not add up"
    );
    assert!(is_newer_than_tested("fedora", "99"));
}

/// a distribution whose family we do not know has no upper threshold: warning
/// about it would be an unreachable branch, since it is rejected first.
#[test]
fn an_unknown_distribution_has_no_upper_threshold() {
    assert!(!is_newer_than_tested("arch", "99"));
}

/// the mandatory commands follow the family: naming one outright was the
/// **first** thing a run on the other family met, failing with a message about
/// the wrong one.
#[test]
fn the_required_commands_follow_the_family() {
    assert_eq!(
        required_commands(OsFamily::Debian),
        ["apt-get", "systemctl"]
    );
    assert_eq!(required_commands(OsFamily::Fedora), ["dnf", "systemctl"]);
}

// --- persistence: the manifest carries the family ---------------------------

/// **the guard on the easiest thing to get wrong.**
///
/// `to_context` builds the rest of the context from defaults. if the family
/// fell through there, every rollback would act as one family — including the
/// other's installations — and no test that does not look at *this* field would
/// notice.
#[test]
fn to_context_propagates_the_recorded_family_not_the_default() {
    let ctx = config_for(OsFamily::Fedora).to_context(false, false, PathBuf::from("/tmp/s.json"));

    assert_eq!(
        ctx.os_family,
        OsFamily::Fedora,
        "the rollback's family is read from the manifest: if the default appears here, \
         `to_context` stopped propagating it and the undo would silently use the wrong \
         commands"
    );
}

#[test]
fn from_context_records_the_family() {
    let ctx = Context {
        os_family: OsFamily::Fedora,
        ..Default::default()
    };
    assert_eq!(
        InstallConfig::from_context(&ctx).os_family,
        OsFamily::Fedora
    );
}

/// a round trip through disk: what is written is what is read back.
#[test]
fn the_family_survives_a_round_trip_through_the_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Fedora));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });
    state.save(&path).expect("save");

    let riletto = InstallState::load(&path).expect("load");
    assert_eq!(
        riletto.config.expect("config presente").os_family,
        OsFamily::Fedora
    );
}

/// **backward compatibility.** a manifest written before the field existed does
/// not declare it, and must read as the default — which is the truth, since
/// every earlier installation used that manager.
///
/// the fixture is the real format, not a reconstruction: making a manifest
/// unreadable makes an already-deployed instance **un-uninstallable**.
#[test]
fn a_manifest_written_before_this_field_reads_as_debian() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let legacy = r#"{
  "completed": [
    { "name": "prepare-opt-root", "snapshot": "CreatedByUs" }
  ],
  "finished": false,
  "config": {
    "odoo_version": "18.0",
    "odoo_version_short": "18",
    "odoo_user": "odoo",
    "db_user": "odoo",
    "db_name": "citest",
    "odoo_home": "/opt/odoo",
    "install_dir": "/opt/odoo/odoo18",
    "port": 8069,
    "odoo_logfile": null,
    "with_nginx": false,
    "sudo_user": "omisen"
  }
}"#;
    std::fs::write(&path, legacy).expect("write");

    let state = InstallState::load(&path).expect("a pre-2.3 manifest must stay readable");
    let config = state.config.expect("config presente");
    assert_eq!(
        config.os_family,
        OsFamily::Debian,
        "without the field the family is Debian: that is what such an installation was"
    );
    assert_eq!(config.db_name, "citest", "the rest reads as before");
}

// --- identity: resuming under another family is not resuming ----------------

/// the family does not *name* an artifact but changes what the recorded names
/// **mean**: a delta written by one manager is not resumable by the other. so
/// it lives in the identity, and the refusal says **which** field differs.
#[test]
fn resuming_with_a_different_family_is_refused_by_name() {
    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Debian));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    let decision = start_decision(&state, &config_for(OsFamily::Fedora), false);

    match decision {
        StartDecision::RefuseIdentityMismatch(differences) => {
            assert!(
                differences
                    .iter()
                    .any(|(field, _, _)| *field == "OS family"),
                "the refusal must name the field that does not match, found: {differences:?}"
            );
        }
        other => panic!("a refusal on a different identity was expected, found {other:?}"),
    }
}

/// …and with the **same** family it resumes as before. an older manifest, read
/// as the default, on a matching machine must stay resumable: the new field
/// must not break installations already in progress.
#[test]
fn resuming_on_the_same_family_still_works() {
    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Debian));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    assert_eq!(
        start_decision(&state, &config_for(OsFamily::Debian), false),
        StartDecision::Resume
    );
}

// --- disagreement: warn, do not decide --------------------------------------

/// the system is read to **warn**, never to act. refusing would make an
/// instance un-uninstallable, and inferring the family from the system would
/// break the rule this field exists for.
#[test]
fn a_family_mismatch_warns_and_does_not_decide() {
    // agreement: no warning.
    assert!(family_mismatch(OsFamily::Debian, Some(OsFamily::Debian)).is_none());
    assert!(family_mismatch(OsFamily::Fedora, Some(OsFamily::Fedora)).is_none());

    // an unidentifiable system: we do not know enough to claim a mismatch.
    assert!(family_mismatch(OsFamily::Debian, None).is_none());

    // disagreement: a warning naming **both**, and which one we proceed with.
    let warning =
        family_mismatch(OsFamily::Debian, Some(OsFamily::Fedora)).expect("a mismatch must be said");
    assert!(
        warning.contains("debian"),
        "it must name the manifest: {warning}"
    );
    assert!(
        warning.contains("fedora"),
        "it must name the system: {warning}"
    );
    assert!(
        warning.contains("proceeding with 'debian'"),
        "it must say the manifest wins, not the system: {warning}"
    );
}

/// the `ID` for the warning is read **without validating**: a rollback must
/// work even on a system we would refuse to install on. uninstalling does not
/// require the machine to still be suitable.
#[test]
fn the_id_for_the_warning_is_read_without_validating() {
    let dir = tempfile::tempdir().expect("tempdir");

    // a release too old to install on, which validation rejects…
    let vecchia = write_os_release(dir.path(), "ID=ubuntu\nVERSION_ID=\"18.04\"\n");
    assert!(check_os_from(&vecchia).is_err());
    // …but the ID still reads, which is what the warning needs.
    assert_eq!(os_id_from(&vecchia).as_deref(), Some("ubuntu"));

    // a missing file gives no answer, and therefore no warning.
    assert_eq!(os_id_from(&dir.path().join("absent")), None);
}
