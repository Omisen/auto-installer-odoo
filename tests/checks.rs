//! the preflight checks: non-mutating, with injectable paths.

use std::io::Write;
use std::path::Path;

use invok::checks::{
    check_disk, check_os_from, check_ports, ensure_root_euid, ensure_sudo_user, format_python,
    format_release, is_newer_than_tested, parse_python_version, ports_to_check,
    python_is_newer_than_tested, untested_python_warning, untested_release_warning, validate_os,
    CheckError, OsInfo, NEWEST_TESTED_DEBIAN, NEWEST_TESTED_FEDORA, NEWEST_TESTED_PYTHON,
    NEWEST_TESTED_UBUNTU,
};
use invok::distro::OsFamily;

fn write_os_release(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("os-release");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

// --- disk: it must NOT create the directory (C4) ----------------------------

#[test]
fn check_disk_does_not_create_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    // a nested, missing target: the measurement must walk up to an ancestor.
    let target = dir.path().join("opt").join("odoo");
    assert!(!target.exists());

    // a zero threshold always passes; what matters is the absent side effect.
    check_disk(&target, 0).expect("check_disk ok");

    // the C4 fix: no directory was created in order to measure.
    assert!(!target.exists(), "check_disk must NOT create the target");
    assert!(
        !dir.path().join("opt").exists(),
        "check_disk must not create the intermediates either"
    );
}

#[test]
fn check_disk_reports_insufficient_without_creating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("opt").join("odoo");

    // an unreachable threshold errors, still without creating anything.
    let err = check_disk(&target, u64::MAX).expect_err("it must fail");
    assert!(matches!(err, CheckError::InsufficientDisk { .. }));
    assert!(!target.exists());
}

// --- OS: parsing and version thresholds -------------------------------------

#[test]
fn check_os_supported() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ubuntu = write_os_release(
        dir.path(),
        "ID=ubuntu\nVERSION_ID=\"22.04\"\nVERSION_CODENAME=jammy\n",
    );
    let info = check_os_from(&ubuntu).expect("ubuntu 22.04 ok");
    assert_eq!(info.id, "ubuntu");
    assert_eq!(info.version, "22.04");
    assert_eq!(info.codename.as_deref(), Some("jammy"));

    let debian = write_os_release(
        dir.path(),
        "ID=debian\nVERSION_ID=\"12\"\nVERSION_CODENAME=bookworm\n",
    );
    let info = check_os_from(&debian).expect("debian 12 ok");
    assert_eq!(info.id, "debian");
}

#[test]
fn check_os_rejects_old_and_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");

    let old_ubuntu = write_os_release(dir.path(), "ID=ubuntu\nVERSION_ID=\"20.04\"\n");
    assert!(matches!(
        check_os_from(&old_ubuntu),
        Err(CheckError::UnsupportedVersion { .. })
    ));

    let old_debian = write_os_release(dir.path(), "ID=debian\nVERSION_ID=\"10\"\n");
    assert!(matches!(
        check_os_from(&old_debian),
        Err(CheckError::UnsupportedVersion { .. })
    ));

    // supported, but with its own threshold: this release is below it.
    let fedora = write_os_release(dir.path(), "ID=fedora\nVERSION_ID=\"39\"\n");
    assert!(matches!(
        check_os_from(&fedora),
        Err(CheckError::UnsupportedVersion { .. })
    ));

    // a distribution whose family we do not know is rejected by the single
    // gate.
    let arch = write_os_release(dir.path(), "ID=arch\nVERSION_ID=\"rolling\"\n");
    assert!(matches!(
        check_os_from(&arch),
        Err(CheckError::UnsupportedOs { .. })
    ));

    // a missing file has its own error.
    let missing = dir.path().join("nope").join("os-release");
    assert!(matches!(
        check_os_from(&missing),
        Err(CheckError::OsReleaseNotFound(_))
    ));
}

// --- root and sudo: pure logic, testable unprivileged -----------------------

#[test]
fn root_and_sudo_pure_logic() {
    assert!(ensure_root_euid(0).is_ok());
    assert!(matches!(
        ensure_root_euid(1000),
        Err(CheckError::NotRoot { euid: 1000 })
    ));

    assert!(ensure_sudo_user(Some("alice")).is_ok());
    assert!(matches!(
        ensure_sudo_user(None),
        Err(CheckError::NoSudoUser)
    ));
    assert!(matches!(
        ensure_sudo_user(Some("")),
        Err(CheckError::NoSudoUser)
    ));
}

/// A-V3-15: where **nginx is already serving**, port 80 is not a conflict — it
/// belongs to the program we are about to configure.
///
/// the check demanded a free port every time nginx was requested, making the
/// normal use case impossible: adding an Odoo vhost to an existing reverse
/// proxy, which the nginx steps handle explicitly.
///
/// found while building the nginx CI job (B-V3-7): the area had never been
/// exercised, and the first real run would have stopped here.
///
/// what is checked is the **decision** — which ports to look at — and not the
/// probe's outcome, which depends on the test machine. this test's first
/// version made exactly that mistake and survived its mutation.
#[test]
fn port_80_held_by_a_running_nginx_is_not_a_conflict() {
    // already listening: those ports are not looked at.
    assert_eq!(
        ports_to_check(8069, true, /* nginx_already_serving */ true),
        vec![8069],
        "an nginx already serving is not a conflict with itself"
    );

    // requested but not yet listening: the conflict would be real.
    assert_eq!(ports_to_check(8069, true, false), vec![8069, 80, 443]);

    // without nginx, port 80 concerns nobody.
    assert_eq!(ports_to_check(8069, false, false), vec![8069]);
    assert_eq!(
        ports_to_check(8069, false, true),
        vec![8069],
        "without --with-nginx the state of nginx is irrelevant"
    );
}

/// but if nginx is **not** serving, a conflict stays one: it would not even
/// bind, and saying so at preflight beats finding out at the reload.
#[test]
fn an_occupied_port_still_stops_the_installation() {
    use std::net::TcpListener;

    // a port is really occupied and passed as Odoo's, exercising the probe
    // without binding a privileged one.
    let occupata = TcpListener::bind("127.0.0.1:0").expect("bind");
    let porta = occupata.local_addr().expect("addr").port();

    assert!(
        check_ports(porta, false, false).is_err(),
        "a port held by a third party must stop the installation"
    );
}

// --- A5.3: accept an untested release, but say so ---------------------------

/// the thresholds are open upwards and must stay so: a refusal without evidence
/// blocks the good case, and a blocked installation is a certain harm while the
/// avoided one is hypothetical.
///
/// but "we accept" must not mean "we keep quiet": on a release newer than the
/// ones we really exercise, the user deserves to know.
#[test]
fn a_release_newer_than_the_tested_ones_is_flagged() {
    // the newest exercised release of this family.
    assert!(!is_newer_than_tested("ubuntu", "22.04"));
    assert!(!is_newer_than_tested("ubuntu", "24.04"));
    assert!(is_newer_than_tested("ubuntu", "24.10"));
    assert!(is_newer_than_tested("ubuntu", "26.04"));

    // and of this one.
    assert!(!is_newer_than_tested("debian", "11"));
    assert!(!is_newer_than_tested("debian", "12"));
    assert!(
        is_newer_than_tested("debian", "13"),
        "Debian 13 passes the thresholds and must be flagged: the same scenario that in \
         A5.2 picked an Ubuntu package"
    );
}

/// they stay **accepted**: the report is a warning, not a refusal.
#[test]
fn a_newer_release_is_still_accepted() {
    for (id, version) in [("ubuntu", "26.04"), ("debian", "13")] {
        let info = OsInfo {
            id: id.to_string(),
            version: version.to_string(),
            codename: None,
            family: OsFamily::Debian,
        };
        assert!(
            validate_os(&info).is_ok(),
            "{id} {version} must be accepted: blocking it would be certain damage to \
             avoid a hypothetical one"
        );
        assert!(is_newer_than_tested(id, version), "…but with a warning");
    }
}

/// a distribution we do not handle is already rejected upstream, so giving it
/// an upper threshold would be an unreachable branch.
#[test]
fn an_unsupported_distribution_has_no_upper_threshold() {
    assert!(!is_newer_than_tested("arch", "99"));
}

// --- A-MD-5: the warning names the reader's own family ----------------------

/// the defect in full: the installer printed two families that had nothing to
/// do with the machine, and **not** the one release actually exercised on it —
/// the only useful information at that moment.
///
/// also the guard against its return: any rewrite naming all three families
/// fails here, because the assertion is "names mine and **not** the others".
#[test]
fn the_untested_warning_names_only_the_family_being_installed() {
    // a deliberately high version: this test is about *who* gets named, not
    // where the threshold falls.
    for (id, own, estranee) in [
        ("ubuntu", "Ubuntu", ["Debian", "Fedora"]),
        ("debian", "Debian", ["Ubuntu", "Fedora"]),
        ("fedora", "Fedora", ["Ubuntu", "Debian"]),
    ] {
        let warning = untested_release_warning(id, "99.99").unwrap_or_else(|| {
            panic!("{id} 99.99 is past every threshold: the warning must be there")
        });

        assert!(
            warning.contains(own),
            "on {id} the warning must name {own}, but it says: {warning}"
        );
        for other in estranee {
            assert!(
                !warning.contains(other),
                "on {id} the warning names {other}, which has nothing to do with this \
                 installation — exactly A-MD-5. text: {warning}"
            );
        }
    }
}

/// **the missing link.** another test ties the constants to the CI matrix, but
/// nothing tied the *message* to the constants: they could diverge in silence,
/// and did for seven phases.
///
/// the expected rendering is rebuilt by hand on purpose: reusing the production
/// function would only prove it equals itself.
#[test]
fn the_untested_warning_quotes_the_tested_release_from_the_constants() {
    fn as_the_distro_writes_it((major, minor): (u32, u32)) -> String {
        if minor == 0 {
            format!("{major}")
        } else {
            format!("{major}.{minor:02}")
        }
    }

    for (id, constant) in [
        ("ubuntu", NEWEST_TESTED_UBUNTU),
        ("debian", NEWEST_TESTED_DEBIAN),
        ("fedora", NEWEST_TESTED_FEDORA),
    ] {
        let warning = untested_release_warning(id, "99.99").expect("the warning must be there");
        let expected = as_the_distro_writes_it(constant);
        assert!(
            warning.contains(&expected),
            "on {id} the warning must cite the tested release ({expected}, from the constant \
             {constant:?}) instead of a hand-written number. text: {warning}"
        );
    }
}

/// a warning on an exercised release would be a false alarm, and a false alarm
/// that appears every time teaches people to ignore warnings (A-V3-10).
#[test]
fn no_warning_on_a_release_we_actually_test() {
    for (id, version) in [
        ("ubuntu", "24.04"),
        ("ubuntu", "22.04"),
        ("debian", "12"),
        ("debian", "11"),
        ("fedora", "41"),
        ("fedora", "40"),
        // an unknown family is rejected upstream: nothing to say here.
        ("arch", "99"),
    ] {
        assert_eq!(
            untested_release_warning(id, version),
            None,
            "{id} {version} is among the ones we test (or is none of our business): warning \
             would be a false alarm"
        );
    }
}

/// a version written wrongly inside a warning about versions is the kind of
/// detail that casts doubt on the rest of the message.
///
/// exercised **directly**, because the case that breaks naive formatting is
/// unreachable through today's constants.
#[test]
fn a_release_is_rendered_the_way_the_distribution_writes_it() {
    assert_eq!(format_release((24, 4)), "24.04");
    assert_eq!(format_release((22, 4)), "22.04");
    assert_eq!(
        format_release((25, 10)),
        "25.10",
        "two digits stay two digits"
    );
    assert_eq!(
        format_release((12, 0)),
        "12",
        "nobody writes \"Debian 12.0\""
    );
    assert_eq!(format_release((41, 0)), "41");
}

// --- the Python interpreter (A-MD-7) ----------------------------------------

/// what `python3 --version` really prints, including the forms that are not
/// simply three numbers.
#[test]
fn the_interpreter_version_is_read_from_what_python_actually_prints() {
    assert_eq!(parse_python_version("Python 3.14.0\n"), Some((3, 14)));
    assert_eq!(parse_python_version("Python 3.12.3\n"), Some((3, 12)));
    assert_eq!(
        parse_python_version("Python 3.14.0rc1\n"),
        Some((3, 14)),
        "a release candidate is still that minor"
    );
    assert_eq!(
        parse_python_version("Python 3.13\n"),
        Some((3, 13)),
        "two components alone are a legitimate output"
    );
}

/// output we cannot read gives `None`, **not** a convenient version.
///
/// the difference between "I know it is covered" and "I do not know": a
/// fallback of zero would be below every threshold and would silence the
/// warning exactly when we have no idea what is underneath.
#[test]
fn an_unreadable_version_is_not_a_version() {
    assert_eq!(parse_python_version(""), None);
    assert_eq!(
        parse_python_version("bash: python3: command not found"),
        None
    );
    assert_eq!(parse_python_version("Python"), None);
    assert_eq!(parse_python_version("Python tre.quattordici"), None);
}

/// the threshold answers in **both directions**, boundary included.
///
/// "exercised" means an installation reaches the end on that version, so
/// warning there would be a false alarm.
#[test]
fn only_an_interpreter_newer_than_the_tested_one_is_flagged() {
    assert!(
        python_is_newer_than_tested((3, 14)),
        "3.14 is past the threshold: the case the check exists for"
    );
    assert!(python_is_newer_than_tested((4, 0)));
    assert!(
        !python_is_newer_than_tested(NEWEST_TESTED_PYTHON),
        "on the tested version there is nothing to flag"
    );
    assert!(!python_is_newer_than_tested((3, 12)));
    assert!(!python_is_newer_than_tested((3, 10)));
}

/// the warning names **the Python found and the one exercised**, and says what
/// will break.
///
/// the content is the check's value, not the fact that it fires: the reader has
/// to decide whether to go on, and for that needs to know which piece fails
/// (A-R9-1).
#[test]
fn the_python_warning_names_both_versions_and_what_will_break() {
    let warning = untested_python_warning((3, 14)).expect("3.14 va segnalato");
    assert!(
        warning.contains("3.14"),
        "the warning does not say which Python it found: {warning}"
    );
    assert!(
        warning.contains(&format_python(NEWEST_TESTED_PYTHON)),
        "the warning does not cite the tested version, so there is no telling how far ahead we are: {warning}"
    );
    assert!(
        warning.contains("gevent"),
        "the warning does not name the package that fails: {warning}"
    );
    assert!(
        warning.contains("install-python-requirements"),
        "the warning does not say where it will stop: {warning}"
    );
    assert_eq!(
        untested_python_warning(NEWEST_TESTED_PYTHON),
        None,
        "on the tested version there is no warning to emit"
    );
}

/// two different conventions: the OS release formatter pads, the Python one
/// does not, and reusing the wrong function would misprint the version.
#[test]
fn a_python_version_is_rendered_the_way_python_writes_it() {
    assert_eq!(format_python((3, 14)), "3.14");
    assert_eq!(format_python((3, 9)), "3.9");
    assert_eq!(format_python((4, 0)), "4.0", "here the zero is not dropped");
}
