//! Test di lockfile (G5) e degrado del logging su file (G1).

use std::path::Path;

use odoo_installer::lockfile;
use odoo_installer::logging;

#[test]
fn second_concurrent_lock_is_refused_and_released_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("installer.lock");

    let guard = lockfile::acquire(&path).expect("primo lock acquisito");
    // Una seconda esecuzione (nuovo descriptor sullo stesso file) è rifiutata.
    assert!(
        lockfile::acquire(&path).is_err(),
        "una seconda installazione deve essere rifiutata"
    );

    drop(guard); // RAII: il lock è rilasciato al Drop.
    let _again = lockfile::acquire(&path).expect("dopo il rilascio si può riacquisire");
}

#[test]
fn log_file_open_degrades_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Percorso scrivibile → Some.
    assert!(logging::try_open(&dir.path().join("installer.log")).is_some());
    // Percorso non scrivibile → None (degrada, non panica).
    assert!(
        logging::try_open(Path::new("/proc/nonexistent-dir-xyz/installer.log")).is_none(),
        "un percorso non scrivibile deve degradare a None"
    );
}
