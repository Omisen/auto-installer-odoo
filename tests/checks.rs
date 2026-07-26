//! Test dei preflight checks (Fase 2): non mutanti, path iniettabili.

use std::io::Write;
use std::path::Path;

use odoo_installer::checks::{
    check_disk, check_os_from, ensure_root_euid, ensure_sudo_user, CheckError,
};

fn write_os_release(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("os-release");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

// --- Disco: NON deve creare la directory (C4) --------------------------------

#[test]
fn check_disk_does_not_create_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Target annidato e inesistente: la misura deve risalire all'antenato.
    let target = dir.path().join("opt").join("odoo");
    assert!(!target.exists());

    // required_gb = 0 → passa sempre; ci interessa il non-effetto collaterale.
    check_disk(&target, 0).expect("check_disk ok");

    // Il fix di C4: nessuna directory è stata creata per misurare.
    assert!(!target.exists(), "check_disk NON deve creare il target");
    assert!(
        !dir.path().join("opt").exists(),
        "check_disk NON deve creare neppure gli intermedi"
    );
}

#[test]
fn check_disk_reports_insufficient_without_creating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("opt").join("odoo");

    // Soglia irraggiungibile → errore tipizzato, ma sempre senza creare nulla.
    let err = check_disk(&target, u64::MAX).expect_err("deve fallire");
    assert!(matches!(err, CheckError::InsufficientDisk { .. }));
    assert!(!target.exists());
}

// --- OS: parsing + soglie di versione ---------------------------------------

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

    let fedora = write_os_release(dir.path(), "ID=fedora\nVERSION_ID=\"39\"\n");
    assert!(matches!(
        check_os_from(&fedora),
        Err(CheckError::UnsupportedOs { .. })
    ));

    // File assente → errore dedicato.
    let missing = dir.path().join("nope").join("os-release");
    assert!(matches!(
        check_os_from(&missing),
        Err(CheckError::OsReleaseNotFound(_))
    ));
}

// --- Root / sudo: logica pura, testabile senza privilegi ---------------------

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
