//! [`SystemOps`]: confine sui comandi di sistema privilegiati.
//!
//! Gli step che creano utenti/gruppi o cambiano owner/permessi non chiamano
//! direttamente `useradd`/`chown`: passano da questo trait. In produzione
//! [`RealSystemOps`] esegue i comandi reali; nei test un mock registra *quale*
//! operazione verrebbe eseguita (con quali argomenti, in quale ramo `PreState`)
//! senza toccare il sistema e senza richiedere root.
//!
//! È il modo per soddisfare la testabilità richiesta dalla Fase 3 senza
//! modificare il trait [`crate::step::Step`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::StepError;

/// Che cosa c'è a un path, guardato **senza seguire i symlink**.
///
/// Esiste perché `symlink_exists` risponde `true` a troppe domande diverse: un
/// symlink, un file regolare e una directory sono tutti «esiste», ma solo il
/// primo si può rimuovere e ricreare identico. Distinguere è la differenza fra
/// ripristinare la configurazione di un cliente e distruggerla (A-V3-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    /// Non c'è nulla.
    Absent,
    /// Un link simbolico, con il target che punta *così com'è scritto*.
    Symlink { target: PathBuf },
    /// Un file regolare: ha un contenuto, e quel contenuto è di qualcuno.
    RegularFile,
    /// Directory, socket, device… o un symlink il cui target non è leggibile.
    /// Non sappiamo trattarlo: chi lo riceve deve astenersi.
    Other,
}

/// Owner numerico di un path (uid/gid), serializzabile per la persistenza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerId {
    pub uid: u32,
    pub gid: u32,
}

/// Stato rilevato dei sorgenti Odoo in una directory target.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OdooSourceState {
    /// La directory non esiste.
    #[default]
    Absent,
    /// Clone git presente, sul branch indicato.
    GitRepo { branch: String },
    /// Directory con `odoo-bin` ma senza `.git` (es. estratta da tarball).
    TarballPresent,
    /// Directory esistente ma non valida (né git corretto né `odoo-bin`).
    InvalidDir,
}

/// Modalità dei file privati scritti dall'installer: solo il proprietario.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Timeout di default delle operazioni di **rete**, in secondi.
///
/// 300s = 5 minuti: abbondante per un `clone --depth 5` di Odoo o per il `.deb`
/// di wkhtmltopdf (~15 MB) anche su una linea lenta, ma abbastanza corto perché
/// un mirror che non chiude mai la connessione produca un errore invece di far
/// sembrare l'installer bloccato. Il valore non è sacro: è il compromesso fra
/// "non troncare un download legittimo" e "non far aspettare venti minuti un
/// cliente". Chi ha una linea davvero lenta lo alza con
/// [`NETWORK_TIMEOUT_ENV`].
pub const DEFAULT_NETWORK_TIMEOUT_SECS: u64 = 300;

/// Variabile d'ambiente che sovrascrive [`DEFAULT_NETWORK_TIMEOUT_SECS`].
/// Il valore `0` disabilita del tutto il timeout (attesa indefinita, come prima
/// di R2). Un valore non numerico viene ignorato e si usa il default.
pub const NETWORK_TIMEOUT_ENV: &str = "ODOO_NETWORK_TIMEOUT_SECS";

/// Intervallo di polling dell'uscita del processo mentre si attende il timeout.
///
/// 50 ms: irrilevante rispetto a timeout dell'ordine dei minuti, e sufficiente
/// a non introdurre ritardi percepibili sui comandi rapidi.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Timeout corrente per le operazioni di rete; `None` = nessun timeout.
///
/// # Perché solo la rete
///
/// Un timeout è utile dove l'attesa può essere **infinita e infruttuosa** — una
/// connessione appesa non progredisce mai. Non lo è dove l'attesa è lunga ma
/// legittima, e lì sarebbe dannoso:
/// - `odoo-bin -i base` e `pip install -r requirements.txt` sono locali e
///   possono durare parecchi minuti su una macchina piccola: un timeout li
///   ucciderebbe a metà di un'installazione perfettamente valida;
/// - `apt-get` è il caso più delicato: può attendere legittimamente un lock
///   `dpkg` tenuto da `unattended-upgrades`, e ucciderlo a metà transazione
///   lascia il database dpkg in stato semi-configurato — un danno **peggiore**
///   dell'attesa, e fuori dal perimetro che il nostro rollback sa riparare.
///   Su un'attesa apt l'utente può sempre interrompere con Ctrl-C; su una
///   transazione dpkg troncata no.
///
/// Restano quindi coperte le tre operazioni che parlano con un host remoto:
/// `git clone`, il download del tarball di fallback e il download del `.deb` di
/// wkhtmltopdf.
pub fn network_timeout() -> Option<Duration> {
    timeout_from_setting(std::env::var(NETWORK_TIMEOUT_ENV).ok().as_deref())
}

/// La politica del timeout, **pura**: come un valore testuale (o la sua
/// assenza) diventa un limite. Separata da [`network_timeout`] così i test la
/// verificano senza mutare l'ambiente del processo.
///
/// - assente o non numerico → [`DEFAULT_NETWORK_TIMEOUT_SECS`];
/// - `0` → `None`, nessun timeout (attesa indefinita, comportamento pre-R2);
/// - `n` → `n` secondi.
pub fn timeout_from_setting(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_NETWORK_TIMEOUT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Legge un pipe fino a EOF in un thread dedicato, restituendo il join handle.
fn drain_pipe<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Attende la fine del processo al massimo `limit`; allo scadere lo **uccide**
/// e ritorna [`StepError::Timeout`].
///
/// `std::process::Command` non ha un timeout nativo. Invece di aggiungere una
/// dipendenza (`wait-timeout` installa un handler SIGCHLD globale di processo)
/// si fa la cosa più semplice che regga: polling di `try_wait` fino alla
/// scadenza. Il `Child` non viene mai spostato altrove, quindi `kill()` non ha
/// la corsa del riuso del pid: il processo non è ancora stato raccolto.
///
/// I due pipe sono drenati da thread dedicati. Non è un dettaglio: `git clone`
/// scrive il progresso su stderr e, senza qualcuno che legga, riempirebbe il
/// buffer del pipe e si bloccherebbe — un deadlock **nostro** che il timeout
/// maschererebbe da "rete lenta".
fn output_with_timeout(
    mut command: Command,
    rendered: &str,
    limit: Duration,
) -> Result<std::process::Output, StepError> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.to_string(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;

    let out_reader = drain_pipe(child.stdout.take());
    let err_reader = drain_pipe(child.stderr.take());

    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Scaduto: uccidi e **raccogli** (niente zombie). Chiusi i
                    // pipe, i due reader vedono EOF e terminano da soli.
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(StepError::Timeout {
                        command: rendered.to_string(),
                        secs: limit.as_secs(),
                    });
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(StepError::CommandFailed {
                    command: rendered.to_string(),
                    status: "wait-failed".to_string(),
                    stderr: e.to_string(),
                });
            }
        }
    };

    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

/// Costruisce il path di un file temporaneo privato **nella stessa directory**
/// di `dest` (stesso filesystem → il `move_file` finale è un rename atomico).
///
/// Il nome porta un suffisso casuale: due esecuzioni concorrenti non collidono e
/// un attaccante locale non può pre-piazzare un symlink al path esatto, perché
/// non lo conosce in anticipo. La casualità è però solo *difesa in profondità*:
/// la garanzia vera è [`SystemOps::create_private_file`], che è fail-closed
/// (`O_EXCL | O_NOFOLLOW`) anche se il nome venisse indovinato.
pub fn private_temp_path(dest: &Path, fallback_name: &str) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback_name.to_string());
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.{}.tmp", random_suffix()))
}

/// Come [`private_temp_path`], ma **conserva l'estensione** del nome dato.
///
/// Esiste per un vincolo esterno, non per gusto: `apt-get install <file>` tratta
/// l'argomento come percorso locale solo se termina in `.deb`, altrimenti lo
/// interpreta come nome di pacchetto e fallisce. Un temporaneo `….tmp` avrebbe
/// reso il nome imprevedibile e l'installazione impossibile — l'ha intercettato
/// il test di `install-wkhtmltopdf`, non un ragionamento.
///
/// Il file resta nascosto (punto iniziale) e con un suffisso casuale prima
/// dell'estensione: `wkhtmltox_….deb` → `.wkhtmltox_….<random>.deb`.
pub fn private_temp_path_keeping_extension(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        // Nessuna estensione da preservare: si ricade sulla forma normale.
        _ => return private_temp_path(&dir.join(name), name),
    };
    dir.join(format!(".{stem}.{}.{ext}", random_suffix()))
}

/// Suffisso esadecimale casuale (8 byte) per i nomi temporanei.
///
/// Legge da `/dev/urandom`; se non fosse disponibile degrada su pid + nanosecondi
/// — sufficiente all'*unicità*, che è tutto ciò che serve alla correttezza
/// (l'unicità evita le collisioni; la sicurezza sta in `O_EXCL | O_NOFOLLOW`).
fn random_suffix() -> String {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        return buf.iter().map(|b| format!("{b:02x}")).collect();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}{:08x}", std::process::id(), nanos)
}

/// Costruttori puri degli argomenti dei comandi che ricevono un **identifier
/// come argomento posizionale**.
///
/// Ogni nome è preceduto da `--`: anche un valore che iniziasse con `-`
/// (`-foo`, `--help`) viene trattato come operando e mai come flag del comando.
/// È la rete a valle dell'argument-injection; la porta a monte è
/// [`crate::config::validate_identifier`], che vieta il trattino iniziale.
/// Sono funzioni pure proprio per poter asserire il `--` nei test senza root.
pub mod argv {
    use super::UserSpec;

    /// `useradd [opzioni] -- <login>`.
    pub fn useradd(spec: &UserSpec) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if spec.system {
            args.push("--system".to_string());
        }
        if spec.create_home {
            args.push("--create-home".to_string());
        }
        args.push("--home-dir".to_string());
        args.push(spec.home.to_string_lossy().into_owned());
        if spec.user_group {
            args.push("--user-group".to_string());
        }
        args.push("--shell".to_string());
        args.push(spec.shell.clone());
        args.push("--".to_string());
        args.push(spec.name.clone());
        args
    }

    /// `userdel -- <login>`. **Mai** `-r`: la home è di `PrepareOptRoot`.
    pub fn userdel(user: &str) -> Vec<String> {
        vec!["--".to_string(), user.to_string()]
    }

    /// `groupdel -- <group>`.
    pub fn groupdel(group: &str) -> Vec<String> {
        vec!["--".to_string(), group.to_string()]
    }

    /// `sudo -Hiu postgres -- createdb --owner <owner> -- <db>`.
    pub fn createdb(owner: &str, db: &str) -> Vec<String> {
        vec![
            "-Hiu".to_string(),
            "postgres".to_string(),
            "--".to_string(),
            "createdb".to_string(),
            "--owner".to_string(),
            owner.to_string(),
            "--".to_string(),
            db.to_string(),
        ]
    }

    /// `sudo -Hiu postgres -- dropdb --if-exists --force -- <db>`.
    pub fn dropdb(db: &str) -> Vec<String> {
        vec![
            "-Hiu".to_string(),
            "postgres".to_string(),
            "--".to_string(),
            "dropdb".to_string(),
            "--if-exists".to_string(),
            "--force".to_string(),
            "--".to_string(),
            db.to_string(),
        ]
    }

    /// `getent passwd -- <user>`.
    pub fn getent_passwd(user: &str) -> Vec<String> {
        vec!["passwd".to_string(), "--".to_string(), user.to_string()]
    }
}

/// Specifica per la creazione di un utente di sistema (argomenti di `useradd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub name: String,
    pub home: PathBuf,
    pub system: bool,
    pub create_home: bool,
    pub user_group: bool,
    pub shell: String,
}

/// Operazioni di sistema mutanti/privilegiate, dietro un confine testabile.
///
/// **Nota di sicurezza:** `delete_user` NON rimuove mai la home (nessun `-r`).
/// La rimozione della home `/opt/odoo` è di competenza esclusiva dello step
/// che l'ha creata (`PrepareOptRoot`), che gira dopo nell'ordine inverso.
pub trait SystemOps {
    fn user_exists(&self, user: &str) -> bool;
    fn path_exists(&self, path: &Path) -> bool;
    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError>;
    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError>;

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError>;
    /// Rimuove l'utente (e il suo gruppo primario). **Mai** `-r`: la home non è
    /// di competenza di questo comando.
    fn delete_user(&self, user: &str) -> Result<(), StepError>;
    fn delete_group(&self, group: &str) -> Result<(), StepError>;

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError>;
    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError>;
    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError>;
    fn mkdir(&self, path: &Path) -> Result<(), StepError>;
    fn rmdir(&self, path: &Path) -> Result<(), StepError>;

    // --- apt / dpkg (Fase 4) -------------------------------------------------
    /// `true` se il pacchetto risulta `install ok installed` a `dpkg-query`.
    fn dpkg_is_installed(&self, pkg: &str) -> bool;
    /// `apt-get update`: riscarica gli indici dei repository.
    ///
    /// È una mutazione (tocca `/var/lib/apt/lists`), quindi vive **solo** dentro
    /// un `run` — mai in uno `snapshot`, che non muta per invariante (C4).
    /// Non ha undo: un indice aggiornato non cambia nulla di ciò che è
    /// installato, è la cache di ciò che *si potrebbe* installare. Come un
    /// `git fetch`.
    fn apt_update(&self) -> Result<(), StepError>;
    /// `true` se apt ha un candidato **reale** per questo nome
    /// (`apt-cache policy` → `Candidate:` diverso da `(none)`).
    ///
    /// È una query, non una mutazione: serve a scegliere *quale* nome installare
    /// quando lo stesso pacchetto ne cambia uno tra release
    /// (`libtiff5-dev` → `libtiff-dev`, A5.1). Senza questa domanda l'unico modo
    /// di scoprire che un nome non esiste più è l'`apt-get install` che fallisce
    /// sull'intero gruppo, cioè a mutazione già iniziata.
    ///
    /// "Reale" è la parola importante: un nome puramente **virtuale** (che
    /// esiste solo come `Provides` di un altro pacchetto) risponde `false` pur
    /// essendo installabile. Vedi [`SystemOps::apt_can_install`] per l'altra
    /// metà della domanda, e la nota sul perché la distinzione conti.
    ///
    /// Risponde `false` anche quando `apt-cache` non è eseguibile o le liste apt
    /// sono vuote: quel caso si distingue con
    /// [`SystemOps::apt_index_is_populated`], non tirando a indovinare.
    fn apt_has_real_candidate(&self, pkg: &str) -> bool;
    /// `true` se apt **saprebbe installare** questo nome
    /// (`apt-get install -s`: simulazione, non muta nulla).
    ///
    /// Risponde `true` anche per un nome puramente virtuale con un solo
    /// fornitore — dove `apt-cache policy` dice `Candidate: (none)` ma
    /// `apt-get install` funziona benissimo. È la domanda che conta per decidere
    /// se un gruppo di alternative è soddisfacibile, ma è **più lenta** (fa
    /// girare il risolutore, ~0.4s per pacchetto): si usa come ripiego quando la
    /// via veloce ha detto no.
    fn apt_can_install(&self, pkg: &str) -> bool;
    /// `true` se l'indice apt contiene almeno un pacchetto
    /// (`apt-cache stats` → `Total package names`).
    ///
    /// Serve a non confondere **cecità** con **assenza**: su una macchina dove
    /// `apt-get update` non è mai stato eseguito ogni interrogazione risponde
    /// "non disponibile", e senza questa domanda un indice vuoto diventerebbe la
    /// diagnosi "questo pacchetto non esiste su questa release" — che è
    /// esattamente il falso positivo A5.1-bis.
    fn apt_index_is_populated(&self) -> bool;
    /// `apt-get install -y --no-install-recommends <pkgs>` (idempotente).
    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError>;
    /// `apt-get purge -y <pkgs>`.
    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError>;
    /// `apt-get autoremove -y`.
    fn apt_autoremove(&self) -> Result<(), StepError>;
    /// `apt-get install -f -y`: installa le dipendenze mancanti e completa la
    /// configurazione dei pacchetti rimasti a metà, riportando `dpkg` in stato
    /// consistente. È il recovery che precede il purge nel rollback.
    fn apt_fix_broken(&self) -> Result<(), StepError>;
    /// `dpkg --configure -a`: riconfigura i pacchetti scompattati ma non
    /// configurati. Copre il caso in cui `apt-get install -f` non basta perché
    /// apt stesso si rifiuta di operare.
    fn dpkg_configure_all(&self) -> Result<(), StepError>;
    /// `apt-get install -y <path.deb>`: installa un `.deb` **locale**
    /// risolvendone le dipendenze.
    ///
    /// Sostituisce `dpkg -i`, che installa il pacchetto ma **non** risolve le
    /// dipendenze: su un sistema minimale il `.deb` resta `unconfigured` e
    /// `dpkg` esce con errore, lasciando l'intero database dpkg in uno stato
    /// che blocca ogni successivo comando apt (bug A-RT-1/A-RT-2, trovati sulla
    /// VM di prova). apt fa le due cose in un colpo solo e non lascia macerie.
    ///
    /// Il path deve essere **assoluto** (o iniziare con `./`): apt tratta come
    /// file solo gli argomenti che contengono una `/`, altrimenti li cerca nei
    /// repository.
    fn apt_install_deb_file(&self, path: &Path) -> Result<(), StepError>;
    /// Versione di `wkhtmltopdf` installata (es. `"0.12.6.1"`), o `None`.
    fn wkhtmltopdf_version(&self) -> Option<String>;

    // --- servizi systemd (Fase 5) --------------------------------------------
    fn service_is_enabled(&self, service: &str) -> bool;
    fn service_is_active(&self, service: &str) -> bool;
    fn service_enable(&self, service: &str) -> Result<(), StepError>;
    fn service_disable(&self, service: &str) -> Result<(), StepError>;
    fn service_start(&self, service: &str) -> Result<(), StepError>;
    fn service_stop(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl restart <service>` (riavvia per applicare la nuova config).
    fn service_restart(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl reload <service>` (ricarica senza downtime).
    fn service_reload(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), StepError>;

    // --- Nginx / firewall (Fase 9) -------------------------------------------
    /// `ln -sf <src> <link>` (symlink idempotente).
    fn create_symlink(&self, src: &Path, link: &Path) -> Result<(), StepError>;
    /// Rimuove un symlink. Idempotente (assente → no-op).
    fn remove_symlink(&self, link: &Path) -> Result<(), StepError>;
    /// `true` se esiste (anche dangling: `test -e`/`test -L`).
    ///
    /// **Attenzione:** risponde `true` anche per un **file regolare** o una
    /// directory — usa `symlink_metadata`, che guarda il path, non la sua natura.
    /// Se la natura conta, usa [`SystemOps::path_kind`]: confondere le due cose è
    /// costato la distruzione di un file di configurazione del cliente (A-V3-5).
    fn symlink_exists(&self, link: &Path) -> bool;
    /// Che cosa c'è a questo path, **senza seguire i symlink**.
    ///
    /// Serve dove la differenza fra «un symlink» e «un file vero» cambia ciò che
    /// è lecito fare: un symlink si può rimuovere e ricreare identico, un file
    /// regolare contiene dati che vanno preservati.
    fn path_kind(&self, path: &Path) -> PathKind;
    /// `ufw` è installato?
    fn ufw_available(&self) -> bool;
    /// `ufw` è attivo (`Status: active`)?
    fn ufw_is_active(&self) -> bool;
    /// La regola (es. `"80/tcp"`) è già presente in `ufw status`?
    fn ufw_rule_exists(&self, rule: &str) -> Result<bool, StepError>;
    /// `ufw allow <rule>`.
    fn ufw_allow(&self, rule: &str) -> Result<(), StepError>;
    /// `ufw delete allow <rule>`.
    fn ufw_delete(&self, rule: &str) -> Result<(), StepError>;
    /// `nginx -t`: la config è valida?
    fn nginx_test(&self) -> bool;

    // --- PostgreSQL (Fase 5) -------------------------------------------------
    /// `true` se il ruolo esiste (`SELECT 1 FROM pg_roles ...`).
    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError>;
    /// `true` se il database esiste (`SELECT 1 FROM pg_database ...`).
    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError>;
    /// Crea il ruolo. `password` è il segreto in chiaro (o `None` = peer auth):
    /// l'escaping e l'invio sicuro (stdin, stderr soppresso) sono qui dentro,
    /// così la password non trapela mai fuori dal confine.
    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError>;
    /// `DROP ROLE IF EXISTS "<role>"`.
    fn pg_drop_role(&self, role: &str) -> Result<(), StepError>;
    /// `createdb --owner <owner> <db>`.
    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError>;
    /// `dropdb --if-exists --force <db>` (chiude le connessioni attive).
    fn dropdb(&self, db: &str) -> Result<(), StepError>;
    /// Elenca i database non-template del cluster (per la cautela sul purge).
    fn pg_list_databases(&self) -> Result<Vec<String>, StepError>;

    // --- sorgenti Odoo (Fase 6, tutto come utente non-root) ------------------
    /// Esegue `sudo -u <user> -- <program> <args>` (privilegio minimo).
    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError>;
    /// `sudo -u <user> -- mkdir -p <path>`.
    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError>;
    /// Rimozione ricorsiva (`rm -rf`) di una dir del **nostro** perimetro
    /// (`<install_dir>/...`). Idempotente: dir assente → no-op.
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError>;
    /// Rileva lo stato dei sorgenti Odoo in `target`.
    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError>;
    /// Un singolo tentativo di `git clone` come `user`.
    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError>;
    /// Fallback: scarica ed estrae il tarball del branch in `target` (come user).
    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError>;
    /// `<venv>/bin/python3` esiste ed è eseguibile?
    fn venv_python_exists(&self, venv: &Path) -> bool;
    /// Il sistema è in grado di **creare** un virtualenv?
    ///
    /// Non è la stessa domanda di "il modulo `venv` esiste": su Debian/Ubuntu il
    /// modulo sta nella stdlib e c'è sempre, mentre `ensurepip` — senza cui
    /// `python3 -m venv` si ferma a metà — arriva col pacchetto `python3-venv`.
    /// L'implementazione chiede di `ensurepip` proprio per questo (A-R6-1).
    fn python_venv_available(&self) -> bool;
    /// `sudo -u <user> -- python3 -m venv <venv>`.
    fn create_venv(&self, user: &str, venv: &Path) -> Result<(), StepError>;
    /// Legge un file di testo (es. requirements.txt).
    fn read_to_string(&self, path: &Path) -> Result<String, StepError>;

    // --- config + init DB (Fase 7) -------------------------------------------
    /// Scrive `content` in un file **privato** (mode `0600` alla creazione): la
    /// master password non è mai leggibile da altri utenti in nessun istante.
    ///
    /// **Semantica: riscrittura in-place di un path già "nostro".** Il file
    /// viene creato se assente e troncato se presente, e un eventuale symlink
    /// **viene seguito** — comportamento voluto per il `.bashrc` dell'utente
    /// installatore, che può legittimamente essere un symlink ai suoi dotfile
    /// (riscriverlo come file regolare distruggerebbe la sua configurazione).
    ///
    /// Per **creare** un file nuovo in una directory non-root (i temporanei
    /// prima del `move_file`) usa invece [`SystemOps::create_private_file`]:
    /// qui il path prevedibile + il follow del symlink sarebbero un vettore
    /// TOCTOU (root che scrive attraverso un symlink pre-piazzato).
    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError>;
    /// **Crea** un file privato (`0600`) a `path`, fail-closed su ogni sorpresa.
    ///
    /// Apre con `O_CREAT | O_EXCL | O_NOFOLLOW`:
    /// - `O_EXCL` → se il path esiste già (file, dir o symlink) l'apertura
    ///   fallisce: non si scrive mai su qualcosa che non abbiamo creato noi;
    /// - `O_NOFOLLOW` → un symlink al path non viene mai seguito.
    ///
    /// È il metodo da usare per i file temporanei scritti da **root** in
    /// directory possedute da altri utenti (es. la install dir, owned `odoo`):
    /// il peggio che un attaccante locale ottiene è un fallimento dello step,
    /// mai una scrittura arbitraria come root né un dirottamento del contenuto
    /// (che include le password). Vedi [`private_temp_path`].
    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError>;
    /// Sposta `src` su `dst` (rename, con fallback copy+remove cross-device).
    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// Copia `src` in `dst` (per il backup).
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// Rimuove un file. Idempotente (`rm -f`): assente → no-op.
    fn remove_file(&self, path: &Path) -> Result<(), StepError>;
    /// `true` se il DB ha già lo schema Odoo (tabella `ir_module_module`).
    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError>;
    /// `sudo -u <user> -- <python> <odoo_bin> -c <conf> -d <db> -i base
    /// --without-demo=all --stop-after-init`.
    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError>;

    // --- control-script + bashrc (Fase 10) -----------------------------------
    /// Home dell'utente da `getent passwd <user>` (campo 6). `None` se assente.
    fn getent_home(&self, user: &str) -> Result<Option<String>, StepError>;
    /// `chown <user>:<user> <path>` (i file restano dell'utente installatore).
    fn chown_to_user(&self, path: &Path, user: &str) -> Result<(), StepError>;
    /// Appende una singola riga a un file (creandolo se assente). **Mai**
    /// riscrivere l'intero file.
    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError>;
}

/// Implementazione reale: esegue i comandi di sistema.
#[derive(Debug, Default)]
pub struct RealSystemOps;

impl RealSystemOps {
    pub fn new() -> Self {
        Self
    }
}

/// Esegue un comando esterno (con eventuali env e un timeout opzionale)
/// mappando l'esito su [`StepError::CommandFailed`].
fn run_command_full(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<(), StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = match timeout {
        // Nessun timeout: `output()` è già la via più semplice e drena i pipe.
        None => command.output().map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?,
        Some(limit) => output_with_timeout(command, &rendered, limit)?,
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Esegue un comando esterno con env aggiuntivi e **senza** timeout.
fn run_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), StepError> {
    run_command_full(program, args, envs, None)
}

/// Esegue un comando esterno senza env aggiuntivi e **senza** timeout.
fn run_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    run_command_full(program, args, &[], None)
}

/// Esegue un comando **di rete** con il timeout corrente ([`network_timeout`]).
///
/// Solo per le operazioni che parlano con un host remoto: `git clone`, il
/// download del tarball, il download del `.deb`. Vedi [`network_timeout`] per
/// il perché le altre operazioni ne restino fuori.
fn run_network_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    run_command_full(program, args, &[], network_timeout())
}

/// Esegue un comando esterno con un timeout **esplicito**.
///
/// È la primitiva su cui poggiano le operazioni di rete, esposta perché il suo
/// comportamento (uccisione del processo alla scadenza, `stderr` catturato,
/// nessun deadlock sui pipe) sia verificabile nei test senza toccare la rete né
/// aspettare minuti.
pub fn run_with_timeout(program: &str, args: &[&str], limit: Duration) -> Result<(), StepError> {
    run_command_full(program, args, &[], Some(limit))
}

/// Adatta gli argomenti costruiti da [`argv`] alla firma di [`run_command`].
fn as_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Esegue `apt-get` con l'ambiente non-interattivo (niente prompt tzdata /
/// needrestart), come il Bash originale.
fn run_apt(args: &[&str]) -> Result<(), StepError> {
    run_command_with_env(
        "apt-get",
        args,
        &[
            ("DEBIAN_FRONTEND", "noninteractive"),
            ("NEEDRESTART_MODE", "a"),
        ],
    )
}

/// `true` se l'output di `apt-cache policy` dichiara un candidato installabile.
///
/// Pura e pubblica per essere verificabile sui casi reali senza avere apt sotto
/// mano: pacchetto disponibile, pacchetto puramente virtuale (`Candidate:
/// (none)`), nome inesistente (nessuna riga `Candidate:`, solo un `N: Unable to
/// locate package`).
pub fn has_installable_candidate(policy_output: &str) -> bool {
    policy_output.lines().any(|line| {
        line.trim()
            .strip_prefix("Candidate:")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "(none)"
            })
            .unwrap_or(false)
    })
}

/// Numero di pacchetti noti ad apt, letto da `apt-cache stats`.
///
/// `None` se la riga non c'è o non è un numero — cioè se non lo sappiamo, che è
/// diverso da "zero". Pura per essere verificabile sull'output reale.
pub fn total_package_names(stats_output: &str) -> Option<u64> {
    for line in stats_output.lines() {
        let Some(value) = line.trim().strip_prefix("Total package names:") else {
            continue;
        };
        // La riga è `Total package names: 163333 (4573 k)`: il primo token è il
        // conteggio, il resto è la dimensione in memoria.
        return value.split_whitespace().next()?.parse().ok();
    }
    None
}

/// Esegue un comando catturandone lo stdout (per query psql/systemctl).
fn capture_command(program: &str, args: &[&str]) -> Result<String, StepError> {
    capture_command_with_env(program, args, &[])
}

/// Come [`capture_command`], con variabili d'ambiente aggiuntive.
///
/// Esiste per `LC_ALL=C`: l'output di `apt-cache` è **localizzato**, e un parser
/// che cerca `Candidate:` su una macchina italiana leggerebbe `Candidato:` e
/// concluderebbe che nessun pacchetto è installabile.
fn capture_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String, StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().map_err(|e| StepError::CommandFailed {
        command: rendered.clone(),
        status: "spawn-failed".to_string(),
        stderr: e.to_string(),
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Esegue un comando passando `input` via **stdin** (non in argv).
///
/// Se `secret` è `true`, lo stderr NON viene incluso nell'errore: psql, in caso
/// di errore, ristampa la riga fallita — che conterrebbe la password. Per i
/// comandi che portano segreti si preferisce perdere il dettaglio diagnostico
/// piuttosto che rischiare un leak nei log.
fn run_command_stdin(
    program: &str,
    args: &[&str],
    input: &str,
    secret: bool,
) -> Result<(), StepError> {
    use std::io::Write;
    use std::process::Stdio;

    let rendered = format!("{program} {}", args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;

    // `take()` per chiudere lo stdin (EOF) dopo la scrittura ed evitare deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| StepError::CommandFailed {
                command: rendered.clone(),
                status: "stdin-write-failed".to_string(),
                stderr: e.to_string(),
            })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "wait-failed".to_string(),
            stderr: e.to_string(),
        })?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = if secret {
            "<output soppresso: potrebbe contenere segreti>".to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr,
        })
    }
}

/// Escape di un literal SQL: raddoppia gli apici singoli (standard SQL).
///
/// Usato per la password del ruolo. Gli identifier (nome ruolo/DB) sono
/// validati come identifier in Fase 1 e vengono comunque double-quotati.
pub fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

/// Converte un `Errno` di nix in `io::Error` per allegarlo a `StepError::Io`.
fn errno_io(e: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}

impl SystemOps for RealSystemOps {
    fn user_exists(&self, user: &str) -> bool {
        nix::unistd::User::from_name(user).ok().flatten().is_some()
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|e| StepError::io(path, e))?;
        Ok(OwnerId {
            uid: meta.uid(),
            gid: meta.gid(),
        })
    }

    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError> {
        let mut entries = std::fs::read_dir(path).map_err(|e| StepError::io(path, e))?;
        Ok(entries.next().is_none())
    }

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError> {
        run_command("useradd", &as_refs(&argv::useradd(spec)))
    }

    fn delete_user(&self, user: &str) -> Result<(), StepError> {
        // MAI `-r`: la home è di competenza di PrepareOptRoot.undo.
        run_command("userdel", &as_refs(&argv::userdel(user)))
    }

    fn delete_group(&self, group: &str) -> Result<(), StepError> {
        run_command("groupdel", &as_refs(&argv::groupdel(group)))
    }

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError> {
        let uid = nix::unistd::User::from_name(owner)
            .ok()
            .flatten()
            .map(|u| u.uid)
            .ok_or_else(|| {
                StepError::Precondition(format!("utente '{owner}' non trovato per chown"))
            })?;
        let gid = nix::unistd::Group::from_name(group)
            .ok()
            .flatten()
            .map(|g| g.gid)
            .ok_or_else(|| {
                StepError::Precondition(format!("gruppo '{group}' non trovato per chown"))
            })?;
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError> {
        let uid = nix::unistd::Uid::from_raw(id.uid);
        let gid = nix::unistd::Gid::from_raw(id.gid);
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| StepError::io(path, e))
    }

    fn mkdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::create_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn rmdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::remove_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn dpkg_is_installed(&self, pkg: &str) -> bool {
        match Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", pkg])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).contains("install ok installed")
            }
            _ => false,
        }
    }

    fn apt_update(&self) -> Result<(), StepError> {
        run_apt(&["update"])
    }

    fn apt_has_real_candidate(&self, pkg: &str) -> bool {
        match capture_command_with_env("apt-cache", &["policy", "--", pkg], &[("LC_ALL", "C")]) {
            Ok(out) => has_installable_candidate(&out),
            // apt-cache assente o in errore: nessuna informazione → nessun
            // candidato reale. Chi chiama non conclude nulla da solo: incrocia
            // con `apt_can_install` e `apt_index_is_populated`.
            Err(_) => false,
        }
    }

    fn apt_can_install(&self, pkg: &str) -> bool {
        // `-s` = simulate: apt calcola la soluzione e non tocca il sistema.
        // Esce 100 con "E: Unable to locate package" se il nome non esiste, 0 se
        // è installabile — anche quando è virtuale con un solo fornitore.
        run_apt(&["install", "-s", "-y", "--no-install-recommends", "--", pkg]).is_ok()
    }

    fn apt_index_is_populated(&self) -> bool {
        match capture_command_with_env("apt-cache", &["stats"], &[("LC_ALL", "C")]) {
            Ok(out) => total_package_names(&out).is_some_and(|n| n > 0),
            // Non riusciamo a chiederlo: trattiamolo come indice non
            // interrogabile. Porta a un messaggio prudente ("aggiorna
            // l'indice"), non a un verdetto di assenza.
            Err(_) => false,
        }
    }

    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut args = vec!["install", "-y", "--no-install-recommends"];
        args.extend_from_slice(pkgs);
        run_apt(&args)
    }

    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut args = vec!["purge", "-y"];
        args.extend_from_slice(pkgs);
        run_apt(&args)
    }

    fn apt_autoremove(&self) -> Result<(), StepError> {
        run_apt(&["autoremove", "-y"])
    }

    fn apt_fix_broken(&self) -> Result<(), StepError> {
        run_apt(&["install", "-f", "-y"])
    }

    fn dpkg_configure_all(&self) -> Result<(), StepError> {
        run_command_with_env(
            "dpkg",
            &["--configure", "-a"],
            &[
                ("DEBIAN_FRONTEND", "noninteractive"),
                ("NEEDRESTART_MODE", "a"),
            ],
        )
    }

    fn apt_install_deb_file(&self, path: &Path) -> Result<(), StepError> {
        // `--` prima del path: un `.deb` in una directory il cui nome inizia
        // con `-` non deve diventare un'opzione (stessa rete di R1).
        let rendered = path.to_string_lossy();
        run_apt(&["install", "-y", "--", &rendered])
    }

    fn wkhtmltopdf_version(&self) -> Option<String> {
        let out = Command::new("wkhtmltopdf").arg("--version").output().ok()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Primo token che sembra una versione (inizia con cifra, ≥ 2 punti).
        text.split_whitespace()
            .find(|tok| {
                tok.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && tok.matches('.').count() >= 2
            })
            .map(|s| s.to_string())
    }

    fn service_is_enabled(&self, service: &str) -> bool {
        matches!(
            Command::new("systemctl").args(["is-enabled", service]).output(),
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "enabled"
        )
    }

    fn service_is_active(&self, service: &str) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", service])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn service_enable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["enable", service])
    }

    fn service_disable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["disable", service])
    }

    fn service_start(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["start", service])
    }

    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["stop", service])
    }

    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["restart", service])
    }

    fn service_reload(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["reload", service])
    }

    fn daemon_reload(&self) -> Result<(), StepError> {
        run_command("systemctl", &["daemon-reload"])
    }

    fn create_symlink(&self, src: &Path, link: &Path) -> Result<(), StepError> {
        let src = src.to_string_lossy();
        let link = link.to_string_lossy();
        run_command("ln", &["-sf", &src, &link])
    }

    fn remove_symlink(&self, link: &Path) -> Result<(), StepError> {
        match std::fs::remove_file(link) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(link, e)),
        }
    }

    fn symlink_exists(&self, link: &Path) -> bool {
        link.symlink_metadata().is_ok()
    }

    fn path_kind(&self, path: &Path) -> PathKind {
        let Ok(meta) = path.symlink_metadata() else {
            return PathKind::Absent;
        };
        if meta.file_type().is_symlink() {
            // Un link il cui target non si legge non è ricreabile identico:
            // trattarlo come "non so cos'è" è meglio che ricrearlo sbagliato.
            return match std::fs::read_link(path) {
                Ok(target) => PathKind::Symlink { target },
                Err(_) => PathKind::Other,
            };
        }
        if meta.is_file() {
            PathKind::RegularFile
        } else {
            PathKind::Other
        }
    }

    fn ufw_available(&self) -> bool {
        Command::new("ufw")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn ufw_is_active(&self) -> bool {
        capture_command("ufw", &["status"])
            .map(|s| s.contains("Status: active"))
            .unwrap_or(false)
    }

    fn ufw_rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let status = capture_command("ufw", &["status"])?;
        Ok(status.contains(rule))
    }

    fn ufw_allow(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["allow", rule])
    }

    fn ufw_delete(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["delete", "allow", rule])
    }

    fn nginx_test(&self) -> bool {
        Command::new("nginx")
            .arg("-t")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError> {
        let sql = format!(
            "SELECT 1 FROM pg_roles WHERE rolname = '{}';",
            escape_sql_literal(role)
        );
        let out = capture_command("sudo", &["-Hiu", "postgres", "--", "psql", "-tAc", &sql])?;
        Ok(!out.trim().is_empty())
    }

    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError> {
        let sql = format!(
            "SELECT 1 FROM pg_database WHERE datname = '{}';",
            escape_sql_literal(db)
        );
        let out = capture_command("sudo", &["-Hiu", "postgres", "--", "psql", "-tAc", &sql])?;
        Ok(!out.trim().is_empty())
    }

    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError> {
        // Identifier double-quotato; password come literal escaped. L'SQL va via
        // stdin e, in caso di errore, lo stderr è soppresso (potrebbe contenerla).
        let sql = match password {
            Some(pw) => format!(
                "CREATE ROLE \"{role}\" WITH LOGIN CREATEDB PASSWORD '{}';",
                escape_sql_literal(pw)
            ),
            None => format!("CREATE ROLE \"{role}\" WITH LOGIN CREATEDB;"),
        };
        run_command_stdin(
            "sudo",
            &["-Hiu", "postgres", "--", "psql", "-v", "ON_ERROR_STOP=1"],
            &sql,
            /* secret */ true,
        )
    }

    fn pg_drop_role(&self, role: &str) -> Result<(), StepError> {
        let sql = format!("DROP ROLE IF EXISTS \"{role}\";");
        run_command(
            "sudo",
            &[
                "-Hiu",
                "postgres",
                "--",
                "psql",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                &sql,
            ],
        )
    }

    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError> {
        run_command("sudo", &as_refs(&argv::createdb(owner, db)))
    }

    fn dropdb(&self, db: &str) -> Result<(), StepError> {
        run_command("sudo", &as_refs(&argv::dropdb(db)))
    }

    fn pg_list_databases(&self) -> Result<Vec<String>, StepError> {
        let out = capture_command(
            "sudo",
            &[
                "-Hiu",
                "postgres",
                "--",
                "psql",
                "-tAc",
                "SELECT datname FROM pg_database WHERE datistemplate = false;",
            ],
        )?;
        Ok(out
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError> {
        let mut full = vec!["-u", user, "--", program];
        full.extend_from_slice(args);
        run_command("sudo", &full)
    }

    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError> {
        let p = path.to_string_lossy();
        run_command("sudo", &["-u", user, "--", "mkdir", "-p", &p])
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError> {
        if target.join(".git").is_dir() {
            let target_str = target.to_string_lossy();
            let branch = capture_command(
                "sudo",
                &[
                    "-u",
                    user,
                    "--",
                    "git",
                    "-C",
                    &target_str,
                    "rev-parse",
                    "--abbrev-ref",
                    "HEAD",
                ],
            )
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
            return Ok(OdooSourceState::GitRepo { branch });
        }
        if target.is_dir() {
            if target.join("odoo-bin").is_file() {
                return Ok(OdooSourceState::TarballPresent);
            }
            return Ok(OdooSourceState::InvalidDir);
        }
        Ok(OdooSourceState::Absent)
    }

    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError> {
        let target_str = target.to_string_lossy();
        let depth_str = depth.to_string();
        // Operazione di rete → timeout. Un tentativo scaduto è un fallimento
        // ritentabile: `CloneOdooRepo` lo tratta come gli altri (retry con
        // backoff, poi fallback tarball).
        run_network_command(
            "sudo",
            &[
                "-u",
                user,
                "--",
                "git",
                "-c",
                "http.version=HTTP/1.1",
                "-c",
                "core.compression=0",
                "clone",
                url,
                "--branch",
                branch,
                "--single-branch",
                "--no-tags",
                "--depth",
                &depth_str,
                &target_str,
            ],
        )
    }

    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError> {
        // Nome **imprevedibile** e file creato da noi prima di passarlo a wget
        // (A-V3-3). Il nome fisso `/tmp/odoo-src.tar.gz` era noto a chiunque
        // leggesse il sorgente: bastava piazzarci un symlink prima che
        // l'installer partisse (root ci scriveva sopra), o sostituire l'archivio
        // fra il download e il `tar` che lo estrae come utente `odoo` — e il
        // tarball, a differenza del `.deb` di wkhtmltopdf, **non ha un checksum
        // atteso** da opporre a un contenuto sostituito.
        //
        // `create_private_file` apre con `O_CREAT | O_EXCL | O_NOFOLLOW`: se a
        // quel path c'è già qualcosa — file, directory o symlink — fallisce
        // invece di scriverci. Dopo, wget riapre il path per nome, ma il file è
        // già nostro, il nome non era indovinabile, e `/tmp` è sticky: nessun
        // altro utente può rimuoverlo per metterci il proprio.
        //
        // Il `chown` non è un dettaglio: il file nasce `0600 root` e a leggerlo
        // sarà il `tar` lanciato come utente `odoo`.
        let tmp = private_temp_path_keeping_extension(&std::env::temp_dir(), "odoo-src.tar.gz");
        self.create_private_file(&tmp, "")?;
        self.chown_named(&tmp, user, user)?;
        let tmp_str = tmp.to_string_lossy().into_owned();
        let target_str = target.to_string_lossy().into_owned();

        // Scarica; poi crea/estrai come utente (i file risultano owned da lui).
        // Solo il download ha un timeout: l'estrazione è locale e su una
        // macchina lenta può durare parecchio in modo del tutto legittimo.
        let outcome = (|| {
            run_network_command("wget", &["-qO", &tmp_str, url])?;
            run_command("sudo", &["-u", user, "--", "mkdir", "-p", &target_str])?;
            run_command(
                "sudo",
                &[
                    "-u",
                    user,
                    "--",
                    "tar",
                    "-xzf",
                    &tmp_str,
                    "-C",
                    &target_str,
                    "--strip-components=1",
                ],
            )?;
            if !target.join("odoo-bin").is_file() {
                return Err(StepError::Precondition(format!(
                    "fallback tarball completato ma odoo-bin assente in {target_str}"
                )));
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp);
        outcome
    }

    fn venv_python_exists(&self, venv: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let python = venv.join("bin").join("python3");
        python
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    fn python_venv_available(&self) -> bool {
        // `import ensurepip`, NON `python3 -m venv --help`.
        //
        // Il modulo `venv` vive in `libpython3.x-stdlib`, che c'è sempre: il suo
        // `--help` risponde 0 anche su un sistema dove creare un virtualenv è
        // impossibile. Il pezzo che il pacchetto `python3-venv` porta davvero è
        // `ensurepip` (verificato: `dpkg -S .../ensurepip/__init__.py` →
        // `python3.12-venv`, mentre `.../venv/__init__.py` → `libpython3.12-stdlib`).
        // Chiedere del modulo sbagliato rendeva questa precondizione un controllo
        // che non poteva fallire, e il fallimento arrivava più tardi come errore
        // grezzo di Python con una directory `sandbox` a metà (A-R6-1).
        Command::new("python3")
            .args(["-c", "import ensurepip"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn create_venv(&self, user: &str, venv: &Path) -> Result<(), StepError> {
        let venv_str = venv.to_string_lossy();
        run_command(
            "sudo",
            &["-u", user, "--", "python3", "-m", "venv", &venv_str],
        )
    }

    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        std::fs::read_to_string(path).map_err(|e| StepError::io(path, e))
    }

    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(content.as_bytes())
            .map_err(|e| StepError::io(path, e))
    }

    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        create_private_file_at(path, content)
    }

    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        if std::fs::rename(src, dst).is_ok() {
            return Ok(());
        }
        // Fallback cross-device: copia poi rimuove la sorgente.
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        std::fs::remove_file(src).map_err(|e| StepError::io(src, e))?;
        Ok(())
    }

    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError> {
        let sql = "SELECT 1 FROM information_schema.tables \
                   WHERE table_schema='public' AND table_name='ir_module_module';";
        let out = capture_command(
            "sudo",
            &["-Hiu", "postgres", "--", "psql", "-d", db, "-tAc", sql],
        )?;
        Ok(!out.trim().is_empty())
    }

    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError> {
        let python = python.to_string_lossy();
        let odoo_bin = odoo_bin.to_string_lossy();
        let conf = conf.to_string_lossy();
        run_command(
            "sudo",
            &[
                "-u",
                user,
                "--",
                &python,
                &odoo_bin,
                "-c",
                &conf,
                "-d",
                db,
                "-i",
                "base",
                "--without-demo=all",
                "--stop-after-init",
            ],
        )
    }

    fn getent_home(&self, user: &str) -> Result<Option<String>, StepError> {
        let output = Command::new("getent")
            .args(argv::getent_passwd(user))
            .output()
            .map_err(|e| StepError::CommandFailed {
                command: format!("getent passwd {user}"),
                status: "spawn-failed".to_string(),
                stderr: e.to_string(),
            })?;
        if !output.status.success() {
            return Ok(None); // utente non trovato
        }
        let line = String::from_utf8_lossy(&output.stdout);
        let home = line
            .trim()
            .split(':')
            .nth(5)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        Ok(home)
    }

    fn chown_to_user(&self, path: &Path, user: &str) -> Result<(), StepError> {
        let u = nix::unistd::User::from_name(user)
            .ok()
            .flatten()
            .ok_or_else(|| {
                StepError::Precondition(format!("utente '{user}' non trovato per chown"))
            })?;
        // gruppo omonimo se esiste, altrimenti gruppo primario dell'utente.
        let gid = nix::unistd::Group::from_name(user)
            .ok()
            .flatten()
            .map(|g| g.gid)
            .unwrap_or(u.gid);
        nix::unistd::chown(path, Some(u.uid), Some(gid))
            .map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        writeln!(file, "{line}").map_err(|e| StepError::io(path, e))
    }
}

/// Confine per i download di rete, separato da [`SystemOps`] così è mockabile
/// nei test senza toccare la rete.
pub trait Downloader {
    /// Scarica `url` in `dest`. La verifica di integrità (checksum) è a carico
    /// del chiamante (vedi [`sha256_hex`]): il download NON è fidato di per sé.
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError>;
}

/// Downloader reale via `wget` (già presente tra i prerequisiti bootstrap).
#[derive(Debug, Default)]
pub struct RealDownloader;

impl RealDownloader {
    pub fn new() -> Self {
        Self
    }
}

impl Downloader for RealDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError> {
        // Il file di destinazione lo creiamo **noi**, fail-closed, prima di
        // dare il path a wget (A-V3-3): `wget -O` apre per nome e segue i
        // symlink, quindi da solo scriverebbe volentieri dove punta un link
        // piazzato da altri. Con `O_EXCL | O_NOFOLLOW` un path già occupato —
        // da un file, una directory o un symlink — fa fallire il download
        // invece di dirottarlo.
        create_private_file_at(dest, "")?;
        let rendered = dest.to_string_lossy();
        // Rete → timeout. Un download troncato dal kill non è comunque
        // installabile: il chiamante verifica il checksum (fail-closed) e
        // rimuove il file parziale.
        run_network_command("wget", &["-q", "-O", &rendered, url])
    }
}

/// Crea un file privato (`0600`) fail-closed: `O_CREAT | O_EXCL | O_NOFOLLOW`.
///
/// È il corpo di [`SystemOps::create_private_file`], estratto come funzione
/// libera perché serve anche a chi non ha un `SystemOps` sotto mano — cioè al
/// [`RealDownloader`], che deve poter creare **lui** il file di destinazione
/// prima di consegnarne il path a `wget` (A-V3-3). Una sola implementazione
/// della primitiva delicata: se cambia, cambia per tutti.
pub fn create_private_file_at(path: &Path, content: &str) -> Result<(), StepError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL
        .custom_flags(nix::libc::O_NOFOLLOW)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|e| StepError::io(path, e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| StepError::io(path, e))
}

/// Calcola lo SHA-256 di un file come stringa esadecimale minuscola.
pub fn sha256_hex(path: &Path) -> Result<String, StepError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| StepError::io(path, e))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| StepError::io(path, e))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
