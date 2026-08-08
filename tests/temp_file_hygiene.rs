//! A-V3-3: nessun file temporaneo con nome prevedibile in una directory condivisa.
//!
//! Due guardie **strutturali** (leggono i sorgenti) e una unitaria sull'helper.
//!
//! Perché strutturali: il mock non può accorgersi di *dove* si scrive. Un test
//! su `MockSystemOps` vede che il file è stato creato e con quale contenuto, non
//! che il path stava in `/tmp` con un nome scritto nel sorgente e quindi noto a
//! chiunque. Quella parte la vede solo un `grep` — ed è la stessa forma di
//! difesa già adottata in R6-hotfix-2 per la precondizione del venv.

use std::fs;
use std::path::{Path, PathBuf};

use invok::system_ops::{private_temp_path, private_temp_path_keeping_extension};

/// Tutti i `.rs` sotto `dir`, ricorsivamente, come (percorso relativo, contenuto).
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

/// **La forma esatta del difetto**: prendere la directory temporanea condivisa e
/// attaccarci un nome fisso.
///
/// `std::env::temp_dir()` di per sé va bene — è la directory che si passa a uno
/// step come parametro iniettabile. Quello che non va bene è
/// `temp_dir().join("qualcosa")`: `/tmp` è world-writable, e un nome scritto nel
/// sorgente è noto a chiunque possa leggerlo. Da lì un utente locale può
/// piazzare un symlink prima che l'installer parta, oppure sostituire il file
/// nella finestra fra la scrittura di root e la lettura dell'utente `odoo` — che
/// per i requirements di pip significa far installare pacchetti arbitrari nel
/// venv, cioè esecuzione di codice come il proprietario del database.
///
/// I nomi vanno costruiti con `private_temp_path` /
/// `private_temp_path_keeping_extension`, che aggiungono un suffisso casuale.
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

/// Ogni scrittura di contenuto negli step passa da `SystemOps`.
///
/// Non è pedanteria architetturale: `SystemOps` è dove vivono le primitive
/// fail-closed (`create_private_file`, con `O_EXCL | O_NOFOLLOW`) ed è l'unico
/// confine che i test su mock possono osservare. Uno `std::fs::write` diretto
/// segue i symlink, tronca quello che trova, e non compare in nessun log di
/// operazioni — che è esattamente com'erano scritti i due requirements di pip
/// prima di R9.
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

// --- l'helper che conserva l'estensione --------------------------------------

/// `apt-get install <file>` riconosce un percorso locale **solo** se termina in
/// `.deb`: un temporaneo `….tmp` avrebbe reso il nome imprevedibile e
/// l'installazione impossibile. Il vincolo è esterno e non negoziabile, quindi
/// va presidiato.
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

/// Un nome senza estensione ricade sulla forma normale invece di produrre
/// qualcosa di malformato (niente punto finale, niente estensione inventata).
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

/// I due helper producono nomi **nascosti** e non indovinabili. È la proprietà
/// da cui dipende tutto il resto: `O_EXCL` protegge dal path già occupato, il
/// nome casuale toglie all'attaccante la possibilità di sapere quale path
/// occupare.
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

// --- il downloader crea lui il file, fail-closed ------------------------------

/// `RealDownloader` deve creare **lui** il file di destinazione, con
/// `O_CREAT | O_EXCL | O_NOFOLLOW`, prima di consegnarne il path a `wget`.
///
/// `wget -O <path>` apre per nome e segue i symlink: da solo scriverebbe
/// volentieri dove punta un link piazzato da altri. La verifica del checksum,
/// che è la difesa sul *contenuto*, arriva dopo — troppo tardi per un file di
/// sistema già sovrascritto.
///
/// Il test non usa la rete e non ne ha bisogno: se il path è già occupato,
/// l'errore deve arrivare **prima** che wget venga eseguito. Si distingue dal
/// caso "wget ha fallito" guardando la forma dell'errore — `Io` contro
/// `CommandFailed` — che è l'unico modo di provare che il comando non è mai
/// partito.
#[test]
fn the_downloader_refuses_a_destination_that_already_exists() {
    use invok::error::StepError;
    use invok::system_ops::{Downloader, RealDownloader};

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("gia-occupato.deb");
    fs::write(&dest, b"contenuto di qualcun altro").expect("write");

    // URL che fallirebbe subito, se mai venisse contattato.
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

/// Stessa difesa contro un **symlink**: `O_NOFOLLOW` non lo segue, quindi il
/// bersaglio resta intatto. È il vettore classico in una directory condivisa.
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
