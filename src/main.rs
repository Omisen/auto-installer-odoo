//! Entry point: dispatch fra i due comandi dell'installer.
//!
//! - **senza sottocomando** → installazione. Flusso: parse CLI → carica `.env`
//!   (se presente) → prompt interattivi (se TTY) → risolvi la cascata → conferma
//!   password 'admin' se serve → costruisci il [`Context`] → preflight → esegui
//!   gli step con rollback automatico in caso di errore.
//! - **`rollback`** (alias `uninstall`) → annulla un'installazione a partire
//!   dallo stato persistito. Vedi [`odoo_installer::rollback`].

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::Parser;

use odoo_installer::checks::{self, OsInfo};
use odoo_installer::cli::{Cli, Command, RollbackArgs};
use odoo_installer::config::{self, AdminConfirm, RawConfig, ResolvedConfig};
use odoo_installer::context::Context;
use odoo_installer::engine::{dry_run_plan, Installer};
use odoo_installer::progress::{IndicatifReporter, LogReporter, ProgressReporter};
use odoo_installer::prompt;
use odoo_installer::rollback::{
    self, ConfirmationGate, InstallStatus, RollbackReport, UndoOutcome,
};
use odoo_installer::state::{self, InstallConfig, InstallState, StartDecision};
use odoo_installer::steps;

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Rollback(args)) => run_rollback(args),
        // Nessun sottocomando: installazione, come è sempre stato.
        None => run_install(&cli),
    }
}

// --- Installazione -----------------------------------------------------------

fn run_install(cli: &Cli) -> Result<()> {
    // Logging TTY + file (degrada senza root; niente file in dry-run). Il guard
    // va tenuto in vita per tutta l'esecuzione.
    let _log_guard = odoo_installer::logging::init(cli.dry_run);

    let interactive = prompt::is_interactive();

    // Sorgenti grezze: CLI e (opzionale) file .env — parsato, mai eseguito.
    let cli_raw = RawConfig::from_cli(cli);
    let env_raw = match &cli.config {
        Some(path) => {
            let raw = config::parse_env_file(path).map_err(|e| anyhow!(e))?;
            tracing::info!(config = %path.display(), "configurazione .env caricata");
            raw
        }
        None => RawConfig::default(),
    };

    // Prompt interattivi solo per i campi non passati da CLI (vuoto se non TTY).
    let prompted = if interactive {
        prompt::collect(&cli_raw, &env_raw)?
    } else {
        tracing::info!("input interattivo non disponibile: uso CLI, .env e default finali");
        RawConfig::default()
    };

    // Cascata + validazione (pura).
    let resolved = ResolvedConfig::resolve(&cli_raw, &env_raw, &prompted, interactive)
        .map_err(|e| anyhow!(e))?;

    // Conferma interattiva della password debole 'admin' (l'hard-stop
    // non-interattivo è già stato applicato dentro `resolve`).
    if let AdminConfirm::ConfirmNeeded =
        config::check_admin_password(resolved.admin_passwd.expose(), interactive)?
    {
        tracing::warn!(
            "La password admin Odoo è impostata al valore debole 'admin': usala solo per demo o ambienti temporanei."
        );
        if !prompt::confirm("Confermi di voler continuare con admin_passwd='admin'?")? {
            bail!("Installazione interrotta: imposta una password admin diversa da 'admin'.");
        }
    }

    // Si lavora sul manifesto che si **trova**: quello corrente, o quello al
    // percorso storico se è l'unico presente (istanza installata prima di R7).
    let state_path = odoo_installer::state::resolve_state_path();
    let mut ctx = Context::from_resolved(resolved, cli.dry_run, state_path);
    ctx.aggressive_rollback = cli.aggressive_rollback;
    ctx.sudo_user = std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty());

    print_configuration(&ctx);

    let mut steps = steps::build_steps();

    // --- dry-run: mostra il piano, non muta nulla, non persiste stato ---------
    // Il piano non ha progresso da mostrare: solo log.
    if ctx.dry_run {
        println!("=== PIANO (dry-run) — nessuna modifica al sistema ===");
        // Il piano **interroga** il sistema: ogni step fa il proprio snapshot, e
        // alcuni chiedono a PostgreSQL passando da `sudo`. Senza privilegi
        // quelle domande non ottengono risposta e lo step viene saltato, quindi
        // il piano è vero ma incompleto — meglio dirlo prima che l'utente lo
        // scopra da una riga di warning in mezzo all'output (A-V3-11).
        if !checks::running_as_root() {
            println!(
                "Nota: senza sudo alcuni step non possono ispezionare il sistema (PostgreSQL, \n\
                 pacchetti installati) e verranno elencati come «snapshot non disponibile».\n\
                 Per il piano completo: sudo odoo-installer --dry-run …\n"
            );
        }
        dry_run_plan(&mut steps, &ctx, &LogReporter);
        println!("=== fine piano (dry-run) ===");
        return Ok(());
    }

    // I preflight girano in DUE gruppi, e l'ordine fra loro non è cosmetico
    // (A-R9-1, trovato dalla CI di integrazione).
    //
    // Prima **chi** sta eseguendo: root e sudo. Serve subito, perché il manifesto
    // è `0600 root` e senza questo controllo un utente non privilegiato
    // leggerebbe «permission denied» su un file di cui non sa nulla.
    checks::check_caller().map_err(|e| anyhow!(e))?;

    // Poi **se** questa esecuzione debba avvenire: manifesto già sul disco →
    // prima installazione, resume o rifiuto (A-V3-1). Legge il disco e basta;
    // l'unica mutazione che ne discende (archiviare il manifesto per `--force`)
    // avviene dopo la conferma e dopo il lock.
    //
    // Sta **prima** dei check d'ambiente, e ci è finito dopo che la CI ha
    // mostrato perché: reinstallare sopra un'istanza funzionante significa avere
    // Odoo in ascolto sulla porta, quindi `check_ports` falliva per primo e
    // questo controllo non veniva raggiunto mai — proprio nello scenario per cui
    // esiste. L'utente si vedeva dire «libera la porta», cioè "ferma Odoo",
    // invece di «esiste già un'installazione: usa rollback o --force».
    // Una porta occupata è una **conseguenza** dell'installazione esistente:
    // diagnosticare la conseguenza al posto della causa manda a sistemare la
    // cosa sbagliata.
    let start = decide_start(&ctx, cli.force)?;

    // Infine **se la macchina può ospitarla**: OS, disco, porte, comandi.
    //
    // Il controllo sulla porta si salta quando a occuparla siamo noi: in un
    // resume il manifesto dice se `setup-systemd` era già passato, e in quel caso
    // il servizio in ascolto è quello che stiamo per finire di installare. Senza
    // questa eccezione un'installazione interrotta dopo lo step 17 non sarebbe
    // più riprendibile — il resume di R8 morirebbe sul suo stesso servizio.
    let port_is_ours = matches!(&start, Start::Resume(state) if state.owns_the_http_port());
    let os_info = run_environment_checks(&ctx, port_is_ours)?;
    // La famiglia si valorizza qui, una volta sola, e da qui in poi è ciò che
    // gli step useranno per decidere comandi e convenzioni — e ciò che finirà
    // nel manifesto per gli `undo` di domani. Mai dedotta step per step.
    ctx.os_family = os_info.family;
    ctx.os_info = Some(os_info);

    // Conferma finale interattiva prima di mutare il sistema.
    let question = match &start {
        Start::Fresh => "Procedere con l'installazione?",
        Start::Resume(_) => "Riprendere l'installazione interrotta?",
        Start::Replace => "Reinstallare da capo, mettendo da parte il manifesto esistente?",
    };
    if interactive && !prompt::confirm(question)? {
        bail!("Installazione annullata dall'utente.");
    }

    print_interrupt_notice(&ctx.state_path);

    // Da qui in poi un Ctrl-C non uccide più il processo: alza un flag che il
    // motore osserva fra uno step e l'altro, e l'installazione si annulla da
    // sé (B-V3-5). Registrato **dopo** la conferma e prima delle mutazioni:
    // prima non servirebbe — non c'è niente da annullare — e il comportamento
    // di default (uscita immediata) è quello che l'utente si aspetta.
    let interrupted = odoo_installer::interrupt::install();

    // Lock esclusivo: impedisce due installazioni simultanee. Il guard rilascia
    // il lock al Drop (successo, errore o panic). Acquisito dopo i check e prima
    // di ogni mutazione.
    let _lock =
        odoo_installer::lockfile::acquire(Path::new(odoo_installer::lockfile::DEFAULT_LOCK_PATH))
            .map_err(|e| anyhow!(e))?;

    // Reporter: barra `indicatif` solo con TTY interattivo (in dry-run non si
    // arriva qui). Costruito **qui**, non prima: `IndicatifReporter` avvia un
    // ticker che ridisegna la barra su stderr, e `inquire` scrive sullo stesso
    // stream — una barra viva durante un prompt gli cancella la riga sotto il
    // naso e l'eco della risposta finisce su un'altra riga. Il progresso nasce
    // quando c'è progresso da mostrare: dopo l'ultima domanda.
    let reporter: Box<dyn ProgressReporter> = if interactive {
        Box::new(IndicatifReporter::new(steps.len()))
    } else {
        Box::new(LogReporter)
    };

    // `--force`: il manifesto precedente si sposta di lato, mai si cancella. Qui
    // e non prima: è una mutazione, e le mutazioni stanno dopo la conferma e
    // dopo il lock.
    let mut installer = match start {
        Start::Fresh => Installer::new(),
        Start::Resume(state) => Installer::resuming_from(*state),
        Start::Replace => {
            let saved = archive_manifest(&ctx.state_path)?;
            tracing::warn!(
                archiviato = %saved.display(),
                "--force: manifesto precedente messo da parte, non cancellato"
            );
            println!(
                "--force: manifesto precedente archiviato in {}.\n\
                 Se quell'installazione aveva creato artefatti, resta l'unica traccia di \
                 cosa rimuovere: conservalo (`odoo-installer rollback --state <file>`).",
                saved.display()
            );
            Installer::new()
        }
    }
    .watching_interrupt(interrupted);
    installer
        .execute_with_reporter(&mut steps, &ctx, reporter.as_ref())
        .map_err(|e| {
            // Il rollback in-process è già stato eseguito dentro `execute`.
            anyhow!(e)
        })?;

    // Installazione riuscita: lo stato viene marcato come concluso e **resta sul
    // disco**. Non è un file stantìo — è il manifesto di disinstallazione: dice
    // cosa abbiamo creato e cosa abbiamo trovato già presente, e senza di esso
    // `odoo-installer rollback` non avrebbe modo di rimuovere questa istanza in
    // un secondo momento (A-R5-1). Fallire qui non annulla un'installazione
    // riuscita: si segnala e si prosegue, con il costo dichiarato.
    // In dry-run non si arriva qui (return anticipato) e nulla è stato scritto.
    if let Err(e) = installer.mark_finished(&ctx) {
        tracing::warn!(
            path = %ctx.state_path.display(),
            error = %e,
            "impossibile marcare lo stato come concluso: l'installazione è comunque \
             riuscita, ma `odoo-installer rollback` potrebbe non poterla disinstallare"
        );
    }

    print_install_summary(&ctx);
    tracing::info!("preparazione completata");
    Ok(())
}

/// Da dove parte questa esecuzione.
///
/// È una **decisione**, non un'azione: `decide_start` la calcola leggendo il
/// disco e nient'altro, così può essere presa prima della conferma interattiva
/// e prima del lock. L'unica mutazione che ne discende — archiviare il manifesto
/// per `--force` — avviene dopo entrambi, insieme a tutte le altre.
enum Start {
    /// Nessun manifesto utile: prima installazione.
    Fresh,
    /// Manifesto parziale compatibile: si riprende da dove si era arrivati.
    /// `Box` perché [`InstallState`] porta l'intero elenco degli step: senza,
    /// ogni variante dell'enum peserebbe quanto il manifesto.
    Resume(Box<InstallState>),
    /// `--force` su un manifesto esistente: si reinstalla da capo dopo averlo
    /// messo da parte.
    Replace,
}

/// Applica la politica di avvio (A-V3-1) e ne formatta l'esito per l'utente.
///
/// La **regola** non sta qui: sta in [`odoo_installer::state::start_decision`],
/// pura e verificabile senza filesystem. Qui restano le due cose che sono
/// davvero di `main`: leggere il manifesto dal disco e trasformare un rifiuto in
/// un messaggio che dica all'utente cosa fare. La separazione è deliberata —
/// A-V3-1 è nato proprio da una decisione che viveva in `main`, dove nessun test
/// arriva.
fn decide_start(ctx: &Context, force: bool) -> Result<Start> {
    let state = InstallState::load(&ctx.state_path).map_err(|e| anyhow!(e))?;
    let richiesta = InstallConfig::from_context(ctx);

    match state::start_decision(&state, &richiesta, force) {
        StartDecision::Fresh => Ok(Start::Fresh),
        StartDecision::Replace => Ok(Start::Replace),

        StartDecision::Resume => {
            println!(
                "Installazione interrotta trovata in {}: {} step già completati, si riprende.\n\
                 Gli step già eseguiti non vengono rifatti e la proprietà degli artefatti \n\
                 registrata allora viene conservata.\n\
                 (Per ricominciare da capo: `sudo odoo-installer rollback`, oppure `--force`.)",
                ctx.state_path.display(),
                state.completed.len()
            );
            Ok(Start::Resume(Box::new(state)))
        }

        StartDecision::RefuseFinished => {
            let istanza = state
                .config
                .as_ref()
                .map(|c| {
                    format!(
                        "Odoo {}, utente '{}', database '{}', in {}",
                        c.odoo_version,
                        c.odoo_user,
                        c.db_name,
                        c.install_dir.display()
                    )
                })
                .unwrap_or_else(|| "configurazione non registrata".to_string());
            bail!(
                "Risulta già un'installazione completata su questa macchina.\n\
                 \n  Manifesto : {}\n  Istanza   : {}\n  Step      : {} registrati\n\
                 \n\
                 Per rimuoverla:                        sudo odoo-installer rollback\n\
                 Per reinstallare sopra (manifesto messo da parte):  --force\n\
                 \n\
                 Proseguire senza una scelta esplicita sovrascriverebbe il manifesto con \
                 artefatti tutti marcati come preesistenti, e questa istanza non sarebbe \
                 più disinstallabile automaticamente.",
                ctx.state_path.display(),
                istanza,
                state.completed.len()
            )
        }

        StartDecision::RefuseIdentityMismatch(differenze) => {
            let elenco: Vec<String> = differenze
                .iter()
                .map(|(campo, prima, ora)| format!("  {campo}: '{prima}' -> '{ora}'"))
                .collect();
            bail!(
                "C'è un'installazione interrotta ({} step) in {}, ma i parametri richiesti \
                 nominano artefatti diversi:\n{}\n\
                 \n\
                 Riprendere così produrrebbe un manifesto a metà fra due istanze, e il \
                 rollback agirebbe in parte sugli artefatti sbagliati.\n\
                 \n\
                 Rilancia con gli stessi parametri per riprendere, oppure \
                 `sudo odoo-installer rollback` per ripulire prima.",
                state.completed.len(),
                ctx.state_path.display(),
                elenco.join("\n")
            )
        }

        StartDecision::RefuseUnknownIdentity => bail!(
            "C'è un'installazione interrotta ({} step) in {}, ma il manifesto non registra \
             la configurazione (formato precedente alla R4): non posso verificare che \
             descriva gli stessi artefatti, quindi non la riprendo.\n\
             \n\
             Usa `sudo odoo-installer rollback --state {}` per ripulire, oppure `--force` \
             per ricominciare mettendo da parte il manifesto.",
            state.completed.len(),
            ctx.state_path.display(),
            ctx.state_path.display()
        ),
    }
}

/// Sposta il manifesto di lato invece di lasciarlo sovrascrivere (`--force`).
///
/// Non si cancella mai: se quell'installazione aveva creato artefatti, questo
/// file è l'**unica** traccia di quali fossero. Il nome porta l'istante per non
/// sovrascrivere un archivio precedente.
fn archive_manifest(path: &Path) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".superseded-{stamp}"));
    let target = path.with_file_name(name);
    std::fs::rename(path, &target)
        .map_err(|e| anyhow!("impossibile archiviare {}: {e}", path.display()))?;
    Ok(target)
}

/// Check d'**ambiente**: la macchina può ospitare questa installazione?
///
/// Non mutano nulla e falliscono prima di ogni mutazione. Sono separati dai
/// check sul chiamante ([`checks::check_caller`]) perché rispondono a una domanda
/// diversa e vanno fatti in un momento diverso: *chi sei* si sa subito, *se la
/// macchina è adatta* ha senso chiederlo solo dopo aver stabilito che questa
/// installazione debba avvenire (A-R9-1).
///
/// `port_is_ours` salta il controllo sulla porta: in un resume il servizio in
/// ascolto è il nostro, e rifiutarsi di proseguire per un conflitto con noi
/// stessi renderebbe irriprendibile proprio l'installazione che stiamo
/// riprendendo.
fn run_environment_checks(ctx: &Context, port_is_ours: bool) -> Result<OsInfo> {
    let os_info = checks::check_os().map_err(|e| anyhow!(e))?;

    let required_gb = std::env::var("MIN_DISK_GB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(checks::DEFAULT_MIN_DISK_GB);
    checks::check_disk(&ctx.odoo_home, required_gb).map_err(|e| anyhow!(e))?;

    if port_is_ours {
        tracing::info!(
            port = ctx.port,
            "porta occupata dal servizio della stessa installazione: controllo saltato (resume)"
        );
    } else {
        // Se nginx sta già servendo, la 80 è sua: non è un conflitto, è il
        // programma che stiamo per configurare (A-V3-15). La domanda si fa solo
        // quando serve davvero.
        let nginx_already_serving = ctx.with_nginx && {
            use odoo_installer::system_ops::SystemOps;
            odoo_installer::system_ops::RealSystemOps::new().service_is_active("nginx")
        };
        checks::check_ports(ctx.port, ctx.with_nginx, nginx_already_serving)
            .map_err(|e| anyhow!(e))?;
    }
    checks::check_commands().map_err(|e| anyhow!(e))?;
    Ok(os_info)
}

/// Stampa il riepilogo della configurazione finale (replica di
/// `print_installation_configuration` del Bash). Non stampa mai la password.
fn print_configuration(ctx: &Context) {
    let admin_line = if ctx.admin_passwd.expose() == "admin" {
        "default 'admin' (consentito solo con conferma esplicita; sconsigliato)".to_string()
    } else {
        "personalizzata".to_string()
    };

    println!();
    println!("================================================================");
    println!("Configurazione finale installazione:");
    println!("  Versione Odoo : {}", ctx.odoo_version);
    println!("  Utente Odoo   : {}", ctx.odoo_user);
    println!("  Database      : {}", ctx.db_name);
    println!("  DB user       : {}", ctx.db_user);
    println!("  Porta HTTP    : {}", ctx.port);
    println!("  Install dir   : {}", ctx.install_dir.display());
    println!(
        "  Nginx         : {}",
        if ctx.with_nginx { "attivo" } else { "no" }
    );
    println!("  Admin passwd  : {admin_line}");
    if ctx.dry_run {
        println!("  Modalità      : dry-run (nessuna mutazione)");
    }
    println!("================================================================");
    println!();
}

/// Riepilogo di fine installazione: cosa è stato creato e come toccarlo (B2).
///
/// Chiude la metà mancante di B2: il rollback aveva già il suo report da R4,
/// l'installazione riuscita finiva invece con una riga di log. Qui l'utente
/// trova, in un posto solo, dove sono le cose e i due comandi che gli servono.
fn print_install_summary(ctx: &Context) {
    let unit = format!("odoo{}", ctx.odoo_version_short);
    println!();
    println!("================================================================");
    println!("Installazione completata.");
    println!();
    println!(
        "  Odoo {}          http://localhost:{}",
        ctx.odoo_version, ctx.port
    );
    println!("  Servizio         {unit} (systemd)");
    println!("  Utente di sistema {}", ctx.odoo_user);
    println!("  Database         {} (ruolo {})", ctx.db_name, ctx.db_user);
    println!("  Sorgenti         {}", ctx.install_dir.display());
    println!(
        "  Config           {}/odoo{}.conf",
        ctx.install_dir.display(),
        ctx.odoo_version_short
    );
    if let Some(logfile) = &ctx.odoo_logfile {
        println!("  Log Odoo         {}", logfile.display());
    } else {
        println!("  Log Odoo         journal (journalctl -u {unit})");
    }
    if ctx.with_nginx {
        println!("  Nginx            reverse proxy attivo su :80");
    }
    println!();
    println!("  Gestione         odoo start|stop|restart|status   (riapri la shell)");
    println!("  Disinstallazione sudo odoo-installer rollback");
    println!();
    println!(
        "Lo stato in {} è il manifesto di disinstallazione: dice cosa\n\
         è stato creato e cosa era già presente. Non rimuoverlo, serve al rollback.",
        ctx.state_path.display()
    );
    println!("================================================================");
    println!();
}

/// Avviso preventivo su cosa fare se l'installazione viene interrotta.
///
/// Il rollback automatico copre i **fallimenti** di uno step, non le
/// interruzioni: un Ctrl-C uccide il processo prima che la gestione dell'errore
/// giri, e il sistema resta con gli artefatti a metà. Da R4 esiste la via
/// d'uscita — il file di stato è scritto dopo ogni step e `odoo-installer
/// rollback` lo consuma — ma serve saperlo *prima* di premere Ctrl-C, non
/// dopo. Un handler SIGINT che stampi il messaggio al momento giusto è
/// pianificato a parte (vedi audit R4).
fn print_interrupt_notice(state_path: &Path) {
    println!(
        "Nota: puoi interrompere con Ctrl-C. L'installer **annulla da sé** quello che ha \n\
         già fatto e il sistema torna come prima.\n\
     \n\
         L'interruzione ha effetto fra uno step e il successivo: lo step in corso viene \n\
         portato a termine, perché fermare a metà un `apt` o l'inizializzazione di un \n\
         database lascerebbe qualcosa di peggio di ciò che si voleva evitare.\n\
     \n\
         Un secondo Ctrl-C esce **subito**: in quel caso il sistema resta a metà e si \n\
         ripulisce con\n\
     \n    sudo odoo-installer rollback\n\n\
         Lo stato necessario è registrato in {} dopo ogni step — vale anche se la \n\
         macchina si spegne.\n",
        state_path.display()
    );
}

// --- Rollback da stato persistito (R4) ---------------------------------------

fn run_rollback(args: &RollbackArgs) -> Result<()> {
    let _log_guard = odoo_installer::logging::init(args.dry_run);

    // Senza `--state`, il manifesto si cerca prima dove lo scriviamo oggi e poi
    // dove lo scriveva la 2.1.0: un'istanza installata da una versione
    // precedente deve restare disinstallabile.
    let state_path = args
        .state
        .clone()
        .unwrap_or_else(odoo_installer::state::resolve_state_path);

    let state = InstallState::load(&state_path).map_err(|e| anyhow!(e))?;

    // Nessuno stato = niente da annullare. È la condizione normale su una
    // macchina pulita o dopo un rollback già completato: non è un errore.
    if state.completed.is_empty() {
        println!(
            "Nessuna installazione da annullare: {} non esiste o non registra alcuno step.",
            state_path.display()
        );
        return Ok(());
    }

    // Senza la configurazione persistita non si sa *quale* utente, *quale*
    // database, *quale* directory annullare — e indovinarli dai default
    // significherebbe rischiare di droppare un database che non abbiamo creato
    // noi. Meglio fermarsi ed elencare cosa c'è, così la pulizia manuale è
    // almeno informata.
    let Some(config) = state.config.clone() else {
        eprintln!(
            "Il file di stato {} è stato scritto da una versione precedente dell'installer e \n\
             non contiene la configurazione dell'installazione (utente, database, directory).\n\
             Senza quei dati un rollback automatico dovrebbe indovinarli, e indovinare qui \n\
             significa rischiare di rimuovere risorse che non abbiamo creato noi.\n\n\
             Step registrati da quell'installazione, in ordine di esecuzione:",
            state_path.display()
        );
        for record in &state.completed {
            eprintln!("  - {}", record.name);
        }
        bail!(
            "rollback automatico non disponibile per questo file di stato: ripulisci a mano \
             gli artefatti elencati, poi rimuovi {}",
            state_path.display()
        );
    };

    // Il manifesto descrive un'installazione di QUESTO installer? (A-V3-8)
    //
    // Da qui in poi ogni percorso che guiderà un `rm -rf`, un `dropdb` o un
    // `userdel` arriva da questo file, e `--state` accetta qualunque percorso.
    // L'unico ancoraggio possibile è un valore che dal file NON arriva:
    // `ODOO_HOME`, che è costante e non sovrascrivibile. Prima di ogni altra
    // cosa, e prima di stampare un riepilogo che darebbe per buono ciò che
    // c'è scritto.
    config.validate_perimeter().map_err(|e| anyhow!(e))?;

    // E il file stesso è una fonte fidata? Solo per un rollback reale: il
    // dry-run stampa soltanto, e poter ispezionare un manifesto copiato altrove
    // è comodo.
    if !args.dry_run {
        state::ensure_trustworthy(&state_path).map_err(|e| anyhow!(e))?;
    }

    // Con quale famiglia stiamo per lavorare, e il sistema è d'accordo?
    //
    // La famiglia si LEGGE dal manifesto e si usa quella: è lei a dire con quali
    // comandi gli artefatti sono stati creati. Il sistema si legge solo per
    // AVVISARE — mai per decidere — perché un manifesto pre-2.3 ricade sul
    // default `Debian` e `--state` accetta il manifesto di un'altra macchina.
    // Loggare la famiglia non è decorativo: un default che nessuno vede è un
    // default che nessuno può smentire.
    tracing::info!(famiglia = %config.os_family, "rollback: famiglia letta dal manifesto");
    let detected = checks::os_id_from(Path::new(checks::OS_RELEASE_PATH))
        .and_then(|id| odoo_installer::distro::OsFamily::from_os_id(&id));
    if let Some(avviso) = odoo_installer::distro::family_mismatch(config.os_family, detected) {
        tracing::warn!("{avviso}");
        eprintln!("Attenzione: {avviso}");
    }

    print_rollback_summary(&state, &state_path, &config, args);

    let interactive = prompt::is_interactive();

    // Conferma prima di un'operazione distruttiva. La politica è una funzione
    // pura (`confirmation_gate`) così è verificabile senza terminale.
    match rollback::confirmation_gate(args.dry_run, args.yes, interactive) {
        ConfirmationGate::Proceed => {}
        ConfirmationGate::Ask => {
            if !prompt::confirm("Procedere con la rimozione elencata sopra?")? {
                bail!("Rollback annullato dall'utente. Nessuna modifica effettuata.");
            }
        }
        ConfirmationGate::RefuseNonInteractive => bail!(
            "il rollback rimuove risorse dal sistema e richiede una conferma. \
             Senza terminale interattivo usa --yes per confermare esplicitamente."
        ),
    }

    // Root e lock solo per un rollback reale: il dry-run non tocca il sistema e
    // deve poter girare anche da utente normale.
    let _lock = if args.dry_run {
        None
    } else {
        checks::check_root().map_err(|e| anyhow!(e))?;
        Some(
            odoo_installer::lockfile::acquire(Path::new(
                odoo_installer::lockfile::DEFAULT_LOCK_PATH,
            ))
            .map_err(|e| anyhow!(e))?,
        )
    };

    let ctx = config.to_context(args.dry_run, args.aggressive_rollback, state_path.clone());

    let report = {
        let reporter: Box<dyn ProgressReporter> = if interactive && !args.dry_run {
            Box::new(IndicatifReporter::new(state.completed.len()))
        } else {
            Box::new(LogReporter)
        };
        rollback::rollback_from_state(&state, &ctx, &steps::real_ops, reporter.as_ref())
    };

    print_rollback_report(&report, args.dry_run);

    if args.dry_run {
        println!("dry-run: nessuna modifica applicata, il file di stato resta al suo posto.");
        return Ok(());
    }

    // Lo stato è consumato solo se il rollback è andato a fondo: se qualcosa è
    // rimasto, il file resta e una seconda esecuzione può riprovare (gli undo
    // sono idempotenti).
    if report.is_clean() {
        match InstallState::clear(&state_path) {
            Ok(()) => println!("Stato consumato: {} rimosso.", state_path.display()),
            Err(e) => tracing::warn!(
                path = %state_path.display(),
                error = %e,
                "rimozione del file di stato fallita: rimuovilo a mano"
            ),
        }
    } else {
        println!(
            "Il file di stato {} NON è stato rimosso: descrive ancora ciò che non è stato \n\
             ripulito. Sistemato il problema, puoi rieseguire `odoo-installer rollback` \n\
             (gli undo sono idempotenti).",
            state_path.display()
        );
    }

    Ok(())
}

/// Riepilogo pre-conferma: cosa verrà annullato e in quale ordine.
fn print_rollback_summary(
    state: &InstallState,
    state_path: &Path,
    config: &odoo_installer::state::InstallConfig,
    args: &RollbackArgs,
) {
    println!();
    println!("================================================================");
    match rollback::install_status(state) {
        InstallStatus::Complete { steps } => {
            println!("Installazione COMPLETA ({steps} step): verrà disinstallata.")
        }
        InstallStatus::Interrupted { done, total } => println!(
            "Installazione INTERROTTA a metà ({done} step su {total} completati):\n\
             verranno ripuliti i residui lasciati da quella esecuzione."
        ),
    }
    println!("Stato di partenza: {}", state_path.display());
    println!();
    println!("Artefatti dell'installazione:");
    println!("  Versione Odoo : {}", config.odoo_version);
    println!("  Utente Odoo   : {}", config.odoo_user);
    println!("  Database      : {}", config.db_name);
    println!("  DB user       : {}", config.db_user);
    println!("  Install dir   : {}", config.install_dir.display());
    println!(
        "  Nginx         : {}",
        if config.with_nginx {
            "configurato"
        } else {
            "no"
        }
    );
    println!();
    println!("Step da annullare, in ordine inverso di esecuzione:");
    for (i, name) in rollback::undo_plan(state).iter().enumerate() {
        println!("  {:>2}. {name}", i + 1);
    }
    println!();
    println!(
        "Verrà rimosso SOLO ciò che l'installer ha creato: risorse preesistenti\n\
         (database, pacchetti, config, utenti già presenti) restano intatte."
    );
    if !args.aggressive_rollback {
        println!(
            "PostgreSQL, Nginx e le utility comuni (git/curl/wget) restano installati:\n\
             usa --aggressive-rollback per purgare anche quelli."
        );
    }
    if args.dry_run {
        println!("Modalità      : dry-run (nessuna mutazione)");
    }
    println!("================================================================");
    println!();
}

/// Report di fine rollback: cosa è stato annullato e — soprattutto — cosa no.
fn print_rollback_report(report: &RollbackReport, dry_run: bool) {
    let verbo = if dry_run { "annullerebbe" } else { "annullati" };
    println!();
    println!("================================================================");
    println!(
        "Rollback: {} {} step su {}.",
        verbo,
        report.undone(),
        report.outcomes.len()
    );

    let residue = report.residue();
    if residue.is_empty() {
        println!("Nessun residuo: il sistema è tornato allo stato precedente.");
        println!("================================================================");
        println!();
        return;
    }

    println!();
    println!(
        "ATTENZIONE — {} step non ripuliti del tutto.",
        residue.len()
    );
    println!("Il rollback è best-effort: prosegue anche quando un undo fallisce, ma");
    println!("ciò che quell'undo non ha rimosso è ancora sul sistema. Da verificare");
    println!("e rimuovere a mano:");
    for item in residue {
        match &item.outcome {
            UndoOutcome::Failed(e) => println!("  - {}: undo fallito ({e})", item.name),
            UndoOutcome::Unknown => println!(
                "  - {}: step sconosciuto a questa versione dell'installer",
                item.name
            ),
            UndoOutcome::NotRehydrated(e) => println!(
                "  - {}: snapshot illeggibile, undo saltato per sicurezza ({e})",
                item.name
            ),
            UndoOutcome::Undone => {}
        }
    }
    println!();
    println!(
        "Il dettaglio completo è nel log ({}).",
        odoo_installer::logging::DEFAULT_LOG_PATH
    );
    println!("================================================================");
    println!();
}
