//! the three local security holes closed in R1 (A2.1, A2.2, A2.4).
//!
//! all *local* vectors: they need an account on the machine, typically the
//! `odoo` user who owns the install dir **root** writes into. every test runs
//! unprivileged, in a tempdir.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use invok::lockfile;
// the family is irrelevant here: these exercise the filesystem primitives,
// which go through neither the package manager nor the distribution
// conventions.
use invok::system_ops::{argv, private_temp_path, RealSystemOps, SystemOps, UserSpec};

// --- A2.1: TOCTOU and symlinks on the private temporary ---------------------

/// the heart of the fix: a symlink planted at the temporary's path is **not**
/// followed. without `O_NOFOLLOW` root would write through it — an arbitrary
/// overwrite, or a hijack of contents that carry the passwords.
#[test]
fn create_private_file_never_writes_through_a_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("vittima.txt");
    std::fs::write(&victim, "contenuto originale").expect("write");

    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::os::unix::fs::symlink(&victim, &tmp).expect("symlink");

    let ops = RealSystemOps::debian();
    let err = ops.create_private_file(&tmp, "admin_passwd = s3cret");
    assert!(err.is_err(), "the open must fail, not follow the link");

    assert_eq!(
        std::fs::read_to_string(&victim).expect("read"),
        "contenuto originale",
        "the target file must not be touched"
    );
    // the symlink is untouched: neither removed nor replaced.
    assert!(std::fs::symlink_metadata(&tmp)
        .expect("lstat")
        .file_type()
        .is_symlink());
}

/// a **dangling** symlink is refused too: without `O_NOFOLLOW` the open would
/// create the file it points at, wherever that is.
#[test]
fn create_private_file_rejects_dangling_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("does-not-exist-yet");
    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::os::unix::fs::symlink(&target, &tmp).expect("symlink");

    let ops = RealSystemOps::debian();
    assert!(ops.create_private_file(&tmp, "segreto").is_err());
    assert!(!target.exists(), "it must not create the link's target");
}

/// `O_EXCL`: an existing regular file is never overwritten. this stops root
/// writing the passwords into a file another user pre-created, and therefore
/// already holds open.
#[test]
fn create_private_file_refuses_a_preexisting_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::fs::write(&tmp, "piazzato prima").expect("write");
    // wide permissions, as an attacker would leave them to read it back.
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).expect("chmod");

    let ops = RealSystemOps::debian();
    assert!(ops
        .create_private_file(&tmp, "admin_passwd = s3cret")
        .is_err());
    assert_eq!(
        std::fs::read_to_string(&tmp).expect("read"),
        "piazzato prima",
        "the pre-existing file must be neither truncated nor rewritten"
    );
}

/// the happy path: created, written, and `0600` from creation — the password is
/// never world-readable, not for an instant.
#[test]
fn create_private_file_creates_0600() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = dir.path().join(".odoo18.conf.tmp");

    let ops = RealSystemOps::debian();
    ops.create_private_file(&tmp, "admin_passwd = s3cret")
        .expect("creazione");

    assert_eq!(
        std::fs::read_to_string(&tmp).expect("read"),
        "admin_passwd = s3cret"
    );
    let mode = std::fs::metadata(&tmp).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "0600 expected, found {mode:o}");
}

/// the temporary's name is unpredictable and does not collide: two concurrent
/// writes to the same destination use different paths and both succeed, and
/// nobody can plant a symlink at a path they do not know.
#[test]
fn private_temp_path_is_unique_and_next_to_dest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("odoo18.conf");

    let a = private_temp_path(&dest, "odoo.conf");
    let b = private_temp_path(&dest, "odoo.conf");
    assert_ne!(a, b, "two temporaries for the same destination must differ");

    // the destination's own directory, so the final move is an atomic rename.
    assert_eq!(a.parent(), dest.parent());
    assert_eq!(b.parent(), dest.parent());

    let ops = RealSystemOps::debian();
    ops.create_private_file(&a, "uno").expect("primo temp");
    ops.create_private_file(&b, "due").expect("secondo temp");
}

// --- A2.2: `--` before positional arguments ---------------------------------

/// the downstream net against argument injection: every command taking a name
/// **positionally** gets a `--` before it, so even a value starting with `-` is
/// an operand and never a flag.
#[test]
fn positional_names_are_preceded_by_double_dash() {
    let spec = UserSpec {
        name: "odoo".to_string(),
        home: std::path::PathBuf::from("/opt/odoo"),
        system: true,
        create_home: true,
        user_group: true,
        shell: "/bin/bash".to_string(),
    };

    let cases: Vec<(&str, Vec<String>, &str)> = vec![
        ("useradd", argv::useradd(&spec), "odoo"),
        ("userdel", argv::userdel("odoo"), "odoo"),
        ("groupdel", argv::groupdel("odoo"), "odoo"),
        ("createdb", argv::createdb("odoo", "odoo_db"), "odoo_db"),
        ("dropdb", argv::dropdb("odoo_db"), "odoo_db"),
        ("getent", argv::getent_passwd("omisen"), "omisen"),
    ];

    for (cmd, args, name) in cases {
        let last = args.last().expect("almeno un argomento");
        assert_eq!(last, name, "{cmd}: the name must be the last argument");
        assert_eq!(
            args.get(args.len() - 2).map(String::as_str),
            Some("--"),
            "{cmd}: the `--` before the positional is missing (args: {args:?})"
        );
    }
}

/// the separator holds with a hostile name too: it stays an operand. the
/// validator would reject it upstream — this is the second line of defence,
/// tested alone.
#[test]
fn double_dash_survives_a_dash_leading_name() {
    let args = argv::createdb("odoo", "--help");
    assert_eq!(args.last().map(String::as_str), Some("--help"));
    assert_eq!(args.get(args.len() - 2).map(String::as_str), Some("--"));

    // the legitimate options preceding the separator are not lost.
    let spec = UserSpec {
        name: "-foo".to_string(),
        home: std::path::PathBuf::from("/opt/odoo"),
        system: true,
        create_home: true,
        user_group: true,
        shell: "/bin/bash".to_string(),
    };
    let args = argv::useradd(&spec);
    assert!(args.contains(&"--system".to_string()));
    assert!(args.contains(&"--create-home".to_string()));
    assert_eq!(args.last().map(String::as_str), Some("-foo"));
    assert_eq!(args.get(args.len() - 2).map(String::as_str), Some("--"));
}

/// `userdel` never acquires `-r`: the home belongs to another step's undo, and
/// the separator must not have introduced it.
#[test]
fn userdel_never_carries_recursive_flag() {
    assert_eq!(
        argv::userdel("odoo"),
        vec!["--".to_string(), "odoo".to_string()]
    );
}

// --- A2.4: the lock file's permissions --------------------------------------

/// the lock file is born `0600`, like the state file and the config
/// temporaries, and the `flock` still works — it acts on the descriptor.
#[test]
fn lockfile_is_created_private() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("installer.lock");

    let guard = lockfile::acquire(&path).expect("lock acquisito");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "0600 expected, found {mode:o}");

    // the lock stays exclusive.
    assert!(lockfile::acquire(&path).is_err());
    drop(guard);
    assert!(lockfile::acquire(Path::new(&path)).is_ok());
}
