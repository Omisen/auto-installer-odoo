//! A-V3-3: no predictably named temporary in a shared directory.
//!
//! two **structural** guards that read the sources, plus unit tests on the
//! helper.
//!
//! structural because the mock cannot notice *where* a write goes: it sees the
//! file and its contents, not that the path was in a world-writable directory
//! under a name written in the source. only a `grep` sees that.

use std::fs;
use std::path::{Path, PathBuf};

use invok::system_ops::{
    private_temp_path, private_temp_path_keeping_extension, tarball_temp_path,
};

/// every `.rs` under `dir`, recursively, as (relative path, contents).
fn rust_sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let content = fs::read_to_string(&path).expect("read");
                out.push((path.display().to_string(), content));
            }
        }
    }
    out
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// **the defect's exact shape**: taking the shared temporary directory and
/// appending a fixed name.
///
/// the directory itself is fine — it is passed to a step as an injectable
/// parameter. joining a literal name onto it is not: the directory is
/// world-writable and a name in the source is known to anyone who can read it.
/// from there a local user can plant a symlink, or replace the file in the
/// window between root's write and another user's read.
///
/// names must come from the helpers that add a random suffix.
#[test]
fn no_fixed_file_name_is_joined_onto_the_shared_temp_dir() {
    for (file, content) in rust_sources(&src_dir()) {
        for (n, line) in content.lines().enumerate() {
            assert!(
                !line.contains("temp_dir().join("),
                "{file}:{} joins a fixed name to the shared temporary directory.\n  {}\n\
                 use private_temp_path (or private_temp_path_keeping_extension when the \
                 extension must be kept): a predictable name there is A-V3-3",
                n + 1,
                line.trim()
            );
        }
    }
}

/// every content write inside a step goes through `SystemOps`.
///
/// not architectural pedantry: that is where the fail-closed primitives live,
/// and the only boundary mock tests can observe. a direct write follows
/// symlinks, truncates what it finds, and appears in no operations log — which
/// is exactly how the pip requirements were written before R9.
#[test]
fn steps_do_not_write_files_behind_system_ops() {
    let steps_dir = src_dir().join("steps");
    for (file, content) in rust_sources(&steps_dir) {
        for (n, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with("///") {
                continue;
            }
            assert!(
                !line.contains("fs::write("),
                "{file}:{} writes a file outside SystemOps.\n  {line}\n\
                 writes go through create_private_file/write_private_file: they are \
                 fail-closed and observable from the tests (A-V3-3)",
                n + 1
            );
        }
    }
}

// --- the helper that preserves the extension --------------------------------

/// the package manager recognises a local path **only** by its extension, so a
/// `.tmp` temporary would have been unpredictable *and* uninstallable. an
/// external, non-negotiable constraint, so it is guarded.
#[test]
fn the_deb_temp_name_stays_installable_and_unpredictable() {
    let dir = Path::new("/tmp");
    let pkg = "wkhtmltox_0.12.6.1-3.jammy_amd64.deb";

    let a = private_temp_path_keeping_extension(dir, pkg);
    let b = private_temp_path_keeping_extension(dir, pkg);

    let name_a = a.file_name().expect("name").to_string_lossy().into_owned();
    assert!(
        name_a.ends_with(".deb"),
        "the manager would not recognise a local path without the .deb extension: {name_a}"
    );
    assert!(
        name_a != pkg && !name_a.ends_with("_amd64.deb"),
        "the name must carry a random suffix before the extension: {name_a}"
    );
    assert_ne!(a, b, "two invocations must not produce the same name");
    assert_eq!(
        a.parent(),
        Some(dir),
        "the file stays in the requested directory"
    );
}

/// a name without an extension falls back to the plain form rather than
/// producing something malformed.
#[test]
fn a_name_without_extension_falls_back_to_the_plain_form() {
    let dir = Path::new("/tmp");
    let generato = private_temp_path_keeping_extension(dir, "no-extension");
    let name = generato
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();

    assert!(
        name.ends_with(".tmp"),
        "the normal format was expected: {name}"
    );
    assert!(name.starts_with(".no-extension."), "{name}");
    assert_eq!(generato.parent(), Some(dir));
}

/// both helpers produce **hidden**, unguessable names. everything else rests on
/// that: `O_EXCL` protects against an occupied path, and the random name takes
/// away the attacker's knowledge of which path to occupy.
#[test]
fn temp_names_are_hidden_and_unpredictable() {
    let dest = Path::new("/tmp/odoo-src.tar.gz");
    let first = private_temp_path(dest, "odoo-src.tar.gz");
    let second = private_temp_path(dest, "odoo-src.tar.gz");

    for path in [&first, &second] {
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with('.'), "name non nascosto: {name}");
        assert_ne!(
            name, "odoo-src.tar.gz",
            "the fixed name is precisely the defect"
        );
    }
    assert_ne!(first, second);
}

// --- the downloader creates the file itself, fail-closed --------------------

/// the downloader must create the destination **itself**, fail-closed, before
/// handing the path to `wget`.
///
/// `wget -O` opens by name and follows symlinks, so on its own it would happily
/// write wherever a planted link points. the checksum check defends the
/// *contents* and comes later — too late for an already-overwritten system
/// file.
///
/// the test needs no network: with the path occupied the error must arrive
/// **before** wget runs, and the error's shape is the only way to prove the
/// command never started.
#[test]
fn the_downloader_refuses_a_destination_that_already_exists() {
    use invok::error::StepError;
    use invok::system_ops::{Downloader, RealDownloader};

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("gia-occupato.deb");
    fs::write(&dest, b"contenuto di qualcun other").expect("write");

    // a URL that would fail at once, were it ever contacted.
    let err = RealDownloader::new()
        .download("http://127.0.0.1:1/pacchetto.deb", &dest)
        .expect_err("an already-occupied path must fail the download");

    assert!(
        matches!(err, StepError::Io { .. }),
        "an I/O error from the fail-closed creation was expected, not a download failure: \
         if the downloader starts, the file has already been truncated. got: {err:?}"
    );
    assert_eq!(
        fs::read(&dest).expect("read"),
        b"contenuto di qualcun other",
        "the pre-existing file must not be touched"
    );
}

/// the same defence against a **symlink**: it is not followed, so the target
/// stays intact. the classic vector in a shared directory.
#[test]
fn the_downloader_does_not_follow_a_symlink_at_the_destination() {
    use invok::system_ops::{Downloader, RealDownloader};

    let dir = tempfile::tempdir().expect("tempdir");
    let bersaglio = dir.path().join("system-file");
    fs::write(&bersaglio, b"not to be overwritten").expect("write");
    let dest = dir.path().join("pacchetto.deb");
    std::os::unix::fs::symlink(&bersaglio, &dest).expect("symlink");

    let _ = RealDownloader::new().download("http://127.0.0.1:1/pacchetto.deb", &dest);

    assert_eq!(
        fs::read(&bersaglio).expect("read"),
        b"not to be overwritten",
        "the symlink's target was touched: O_NOFOLLOW is protecting nothing"
    );
}

// --- A-V3-23: the tarball could never be downloaded -------------------------

/// the file `root` creates and then hands to `odoo` must not be born in the
/// shared temporary directory.
///
/// not a preference: `/tmp` is sticky and world-writable, and
/// `fs.protected_regular` refuses `O_CREAT` on a file owned by **somebody
/// else** there — to root as well, since `may_create_in_sticky()` has no
/// shortcut for `CAP_FOWNER`. so `wget` was denied its own file and the
/// fallback failed every single time, with a `Permission denied` that names
/// root as the one lacking permission.
///
/// measured on the VM, and the three variants isolate the cause exactly:
/// root writing somebody else's file in `/tmp` is refused, the same file owned
/// by root is fine, and the same situation in a non-sticky directory is fine.
#[test]
fn the_source_tarball_is_not_downloaded_into_the_shared_temp_dir() {
    let sources = PathBuf::from("/opt/odoo/odoo18/odoo");
    let tmp = tarball_temp_path(&sources);

    assert!(
        tmp.starts_with(&sources),
        "it must live in the directory that belongs to the user who reads it, and that the \
         undo removes with rm -rf: {}",
        tmp.display()
    );
    assert!(
        !tmp.starts_with(std::env::temp_dir()),
        "a file chowned away from root cannot be created in a sticky world-writable directory"
    );
    // and A-V3-3 still holds: unpredictable name, extension kept.
    assert_eq!(tmp.extension().and_then(|e| e.to_str()), Some("gz"));
    assert_ne!(
        tarball_temp_path(&sources),
        tmp,
        "two calls must not produce the same name"
    );
    let name = tmp.file_name().and_then(|n| n.to_str()).expect("name");
    assert!(name.starts_with('.'), "hidden, like the other temporaries");
}

/// the shared temporary directory has exactly **two** users left, and both are
/// the same one: the injected default of the wkhtmltopdf step.
///
/// frozen on purpose, in the spirit of `tests/apt_packages.rs`. that one is
/// legitimate — the `.deb` stays `root`-owned and is read by `apt` as root, so
/// none of the above applies — but the next temporary written there will have
/// to justify itself here, in front of the reason this test exists.
#[test]
fn the_shared_temp_dir_has_no_new_users() {
    let allowed = ["src/steps/mod.rs"];
    let mut found: Vec<String> = Vec::new();
    for (file, content) in rust_sources(&src_dir()) {
        for (n, line) in content.lines().enumerate() {
            if line.contains("temp_dir()") && !allowed.iter().any(|a| file.ends_with(a)) {
                found.push(format!("{file}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a new temporary in the shared directory. if root writes it and another user reads it, \
         it cannot live there (A-V3-23); if it really can, add the file to `allowed` with the \
         reason:\n{}",
        found.join("\n")
    );
}
