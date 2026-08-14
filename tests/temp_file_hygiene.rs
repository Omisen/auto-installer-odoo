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

use invok::system_ops::{private_temp_path, private_temp_path_keeping_extension};

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
                "{file}:{} attacca un nome fisso alla directory temporanea condivisa.\n  {}\n\
                 Usa private_temp_path (o private_temp_path_keeping_extension se serve \
                 conservare l'estensione): un nome prevedibile in /tmp è A-V3-3",
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
                "{file}:{} scrive un file fuori da SystemOps.\n  {line}\n\
                 Le scritture passano da create_private_file/write_private_file: \
                 sono fail-closed e osservabili dai test (A-V3-3)",
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

    let nome_a = a.file_name().expect("nome").to_string_lossy().into_owned();
    assert!(
        nome_a.ends_with(".deb"),
        "apt non riconoscerebbe un percorso locale senza estensione .deb: {nome_a}"
    );
    assert!(
        nome_a != pkg && !nome_a.ends_with("_amd64.deb"),
        "il nome deve portare un suffisso casuale prima dell'estensione: {nome_a}"
    );
    assert_ne!(a, b, "due invocazioni non devono produrre lo stesso nome");
    assert_eq!(
        a.parent(),
        Some(dir),
        "il file resta nella directory richiesta"
    );
}

/// a name without an extension falls back to the plain form rather than
/// producing something malformed.
#[test]
fn a_name_without_extension_falls_back_to_the_plain_form() {
    let dir = Path::new("/tmp");
    let generato = private_temp_path_keeping_extension(dir, "senza-estensione");
    let nome = generato
        .file_name()
        .expect("nome")
        .to_string_lossy()
        .into_owned();

    assert!(nome.ends_with(".tmp"), "atteso il formato normale: {nome}");
    assert!(nome.starts_with(".senza-estensione."), "{nome}");
    assert_eq!(generato.parent(), Some(dir));
}

/// both helpers produce **hidden**, unguessable names. everything else rests on
/// that: `O_EXCL` protects against an occupied path, and the random name takes
/// away the attacker's knowledge of which path to occupy.
#[test]
fn temp_names_are_hidden_and_unpredictable() {
    let dest = Path::new("/tmp/odoo-src.tar.gz");
    let uno = private_temp_path(dest, "odoo-src.tar.gz");
    let due = private_temp_path(dest, "odoo-src.tar.gz");

    for path in [&uno, &due] {
        let nome = path
            .file_name()
            .expect("nome")
            .to_string_lossy()
            .into_owned();
        assert!(nome.starts_with('.'), "nome non nascosto: {nome}");
        assert_ne!(
            nome, "odoo-src.tar.gz",
            "il nome fisso è proprio il difetto"
        );
    }
    assert_ne!(uno, due);
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
    fs::write(&dest, b"contenuto di qualcun altro").expect("write");

    // a URL that would fail at once, were it ever contacted.
    let err = RealDownloader::new()
        .download("http://127.0.0.1:1/pacchetto.deb", &dest)
        .expect_err("un path già occupato deve far fallire il download");

    assert!(
        matches!(err, StepError::Io { .. }),
        "atteso un errore di I/O dalla creazione fail-closed, non un fallimento di \
         wget: se wget parte, il file è già stato troncato. Ottenuto: {err:?}"
    );
    assert_eq!(
        fs::read(&dest).expect("read"),
        b"contenuto di qualcun altro",
        "il file preesistente non deve essere toccato"
    );
}

/// the same defence against a **symlink**: it is not followed, so the target
/// stays intact. the classic vector in a shared directory.
#[test]
fn the_downloader_does_not_follow_a_symlink_at_the_destination() {
    use invok::system_ops::{Downloader, RealDownloader};

    let dir = tempfile::tempdir().expect("tempdir");
    let bersaglio = dir.path().join("file-di-sistema");
    fs::write(&bersaglio, b"da non sovrascrivere").expect("write");
    let dest = dir.path().join("pacchetto.deb");
    std::os::unix::fs::symlink(&bersaglio, &dest).expect("symlink");

    let _ = RealDownloader::new().download("http://127.0.0.1:1/pacchetto.deb", &dest);

    assert_eq!(
        fs::read(&bersaglio).expect("read"),
        b"da non sovrascrivere",
        "il bersaglio del symlink è stato toccato: O_NOFOLLOW non sta proteggendo nulla"
    );
}
