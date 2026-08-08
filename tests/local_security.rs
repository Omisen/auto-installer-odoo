//! Test dei tre buchi di sicurezza locale chiusi in R1 (audit A2.1/A2.2/A2.4).
//!
//! Sono tutti vettori *locali*: richiedono un utente sulla macchina (tipicamente
//! l'utente `odoo`, che possiede la install dir dove **root** scrive). Girano
//! tutti senza root, in tempdir.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use invok::lockfile;
// La famiglia è indifferente per questi test: esercitano le primitive di
// filesystem (`create_private_file`, `write_private_file`), che non passano né
// dal gestore di pacchetti né dalle convenzioni di distribuzione.
use invok::system_ops::{argv, private_temp_path, RealSystemOps, SystemOps, UserSpec};

// --- A2.1 — TOCTOU / symlink sul temporaneo privato --------------------------

/// Il cuore del fix: un symlink pre-piazzato al path del temp **non** viene
/// seguito. Senza `O_NOFOLLOW`, root scriverebbe attraverso il symlink nel file
/// bersaglio — overwrite arbitrario, o dirottamento del contenuto (che porta le
/// password del config).
#[test]
fn create_private_file_never_writes_through_a_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("vittima.txt");
    std::fs::write(&victim, "contenuto originale").expect("write");

    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::os::unix::fs::symlink(&victim, &tmp).expect("symlink");

    let ops = RealSystemOps::debian();
    let err = ops.create_private_file(&tmp, "admin_passwd = s3cret");
    assert!(err.is_err(), "l'apertura deve fallire, non seguire il link");

    assert_eq!(
        std::fs::read_to_string(&victim).expect("read"),
        "contenuto originale",
        "il file bersaglio non deve essere toccato"
    );
    // Il symlink resta com'era: non lo abbiamo né rimosso né sostituito.
    assert!(std::fs::symlink_metadata(&tmp)
        .expect("lstat")
        .file_type()
        .is_symlink());
}

/// Anche un symlink **dangling** (bersaglio inesistente) è rifiutato: senza
/// `O_NOFOLLOW` l'apertura creerebbe il file puntato, ovunque esso sia.
#[test]
fn create_private_file_rejects_dangling_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("non-esiste-ancora");
    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::os::unix::fs::symlink(&target, &tmp).expect("symlink");

    let ops = RealSystemOps::debian();
    assert!(ops.create_private_file(&tmp, "segreto").is_err());
    assert!(!target.exists(), "non deve creare il bersaglio del link");
}

/// `O_EXCL`: un file regolare già presente al path non viene mai sovrascritto
/// (fail-closed). Impedisce che root scriva le password dentro un file
/// pre-creato — e quindi già aperto/leggibile — da un altro utente.
#[test]
fn create_private_file_refuses_a_preexisting_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = dir.path().join(".odoo18.conf.tmp");
    std::fs::write(&tmp, "piazzato prima").expect("write");
    // Permessi larghi, come li lascerebbe un attaccante per rileggere il file.
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).expect("chmod");

    let ops = RealSystemOps::debian();
    assert!(ops
        .create_private_file(&tmp, "admin_passwd = s3cret")
        .is_err());
    assert_eq!(
        std::fs::read_to_string(&tmp).expect("read"),
        "piazzato prima",
        "il file preesistente non deve essere troncato né riscritto"
    );
}

/// Il ramo felice: file creato, contenuto scritto, mode `0600` fin dalla
/// creazione (la password non è world-readable in nessun istante).
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
    assert_eq!(mode, 0o600, "atteso 0600, trovato {mode:o}");
}

/// Il nome del temp è imprevedibile e non collide: due scritture concorrenti
/// sulla stessa destinazione usano path diversi e riescono entrambe (nessuna
/// delle due inciampa nell'`O_EXCL` dell'altra), e nessuno può pre-piazzare un
/// symlink su un path che non conosce.
#[test]
fn private_temp_path_is_unique_and_next_to_dest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("odoo18.conf");

    let a = private_temp_path(&dest, "odoo.conf");
    let b = private_temp_path(&dest, "odoo.conf");
    assert_ne!(a, b, "due temp per la stessa destinazione devono differire");

    // Stessa directory della destinazione → il move finale è un rename atomico.
    assert_eq!(a.parent(), dest.parent());
    assert_eq!(b.parent(), dest.parent());

    let ops = RealSystemOps::debian();
    ops.create_private_file(&a, "uno").expect("primo temp");
    ops.create_private_file(&b, "due").expect("secondo temp");
}

// --- A2.2 — `--` prima degli argomenti posizionali ---------------------------

/// Rete a valle dell'argument-injection: in ogni comando che riceve un nome
/// come **posizionale**, il nome è preceduto da `--`. Anche un valore che
/// iniziasse con `-` sarebbe trattato come operando, mai come flag.
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
        assert_eq!(last, name, "{cmd}: il nome deve essere l'ultimo argomento");
        assert_eq!(
            args.get(args.len() - 2).map(String::as_str),
            Some("--"),
            "{cmd}: manca il `--` prima del posizionale (args: {args:?})"
        );
    }
}

/// Il `--` regge anche con un nome ostile: resta un operando in coda, non
/// diventa un flag. (Il validatore lo rifiuterebbe a monte — questa è la
/// seconda linea di difesa, testata da sola.)
#[test]
fn double_dash_survives_a_dash_leading_name() {
    let args = argv::createdb("odoo", "--help");
    assert_eq!(args.last().map(String::as_str), Some("--help"));
    assert_eq!(args.get(args.len() - 2).map(String::as_str), Some("--"));

    // `useradd` non perde le opzioni legittime che precedono il `--`.
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

/// `userdel` non acquisisce mai `-r` (protezione critica: la home è di
/// competenza di `PrepareOptRoot.undo`). Il `--` non deve averla introdotta.
#[test]
fn userdel_never_carries_recursive_flag() {
    assert_eq!(
        argv::userdel("odoo"),
        vec!["--".to_string(), "odoo".to_string()]
    );
}

// --- A2.4 — permessi del lock file -------------------------------------------

/// Il lock file nasce `0600`, come lo state file e i temporanei di config, e il
/// `flock` continua a funzionare (opera sul descrittore, non sui permessi).
#[test]
fn lockfile_is_created_private() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("installer.lock");

    let guard = lockfile::acquire(&path).expect("lock acquisito");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "atteso 0600, trovato {mode:o}");

    // Il lock resta esclusivo.
    assert!(lockfile::acquire(&path).is_err());
    drop(guard);
    assert!(lockfile::acquire(Path::new(&path)).is_ok());
}
