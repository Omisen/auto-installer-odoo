//! [`InstallPythonRequirements`] (9c): installa le dipendenze pip nel venv.
//!
//! # Il cuore concettuale: nessun undo proprio
//!
//! I pacchetti pip vivono **dentro** `<install_dir>/sandbox` (il venv). Se il
//! venv è nostro, l'undo di [`CreateVirtualenv`](crate::steps::create_virtualenv)
//! (`rm -rf sandbox`) li rimuove tutti. Quindi l'undo di questo step è un
//! **NO-OP documentato**: sarebbe ridondante e fragile disinstallare i pacchetti
//! uno a uno. Un undo in meno da scrivere, per la ragione giusta.
//!
//! Tutte le install girano come utente **odoo**, dal `pip` del venv.
//!
//! # La cache di pip vive nel nostro perimetro (A-R5-3)
//!
//! `pip` mette la sua cache in `$HOME/.cache/pip`, e l'`$HOME` dell'utente
//! `odoo` è `/opt/odoo` — che è la directory che l'installer, se la trova già
//! esistente, considera `Preexisting` e non tocca mai. Risultato osservato in
//! campo (job Ubuntu di R5): dopo un rollback completo, `/opt/odoo/.cache`
//! restava lì. Non sono dati critici, ma la promessa è "il sistema torna
//! esattamente com'era", e un residuo è un residuo.
//!
//! La correzione è **preventiva**, non una pulizia: `--cache-dir` sposta la
//! cache in `<install_dir>/sandbox/.pip-cache`, cioè dentro il venv, che l'undo
//! di [`CreateVirtualenv`](crate::steps::create_virtualenv) rimuove per intero
//! con un `rm -rf`. Niente nasce fuori dal perimetro, quindi niente va inseguito
//! con euristiche di cancellazione dentro la home del cliente. La cache resta
//! comunque una cache: un secondo giro dell'installer (idempotenza) la ritrova
//! al suo posto e non riscarica le wheel.
//!
//! # Workaround Cython/gevent (fix reale per Odoo 18, da preservare)
//!
//! Cython 3 ha rimosso il tipo `long` di Python 2; gevent (richiesto da Odoo 18)
//! usa ancora quel codice nei `.pyx`. pip costruisce le wheel in un ambiente
//! isolato che ignora il Cython del venv. Soluzione: installare `Cython<3` nel
//! venv, poi gevent con `--no-build-isolation` (usa il Cython locale), poi il
//! resto dei requirements **escludendo** gevent (già installato).
//!
//! # Quale gevent: lo decide pip, non noi (A-R6-3)
//!
//! Il `requirements.txt` di Odoo 18 non pinna **una** versione di gevent: ne
//! pinna quattro, una per versione di Python, e altrettante di greenlet —
//! annotate da Odoo stesso con il nome della release Ubuntu:
//!
//! ```text
//! gevent==21.8.0  ; … python_version == '3.10'              # (Jammy)
//! gevent==24.2.1  ; … python_version >= '3.12' and < '3.13' # (Noble)
//! greenlet==1.1.2 ; … python_version == '3.10'              # (Jammy)
//! greenlet==3.0.3 ; … python_version >= '3.12' and < '3.13' # (Noble)
//! ```
//!
//! La prima versione di questo step estraeva **la prima riga** che iniziasse con
//! `gevent`, buttando via il marker d'ambiente. Su Ubuntu 22.04 è la riga giusta
//! per coincidenza (Python 3.10 è la prima); su 24.04 sceglieva ancora la riga di
//! Jammy, e gevent 21.8.0 **non compila** contro Python 3.12 —
//! `longintrepr.h: No such file` (header reso privato in 3.12) e, per il greenlet
//! che si tira dietro, `PyThreadState has no member 'recursion_limit'`. Nessun
//! setuptools poteva salvarlo: era la versione sbagliata.
//!
//! Il marker veniva rimosso perché `--no-build-isolation` non lo tollera *su
//! argv* — vero, ma è un problema che ci si creava da soli. Passando un **file**
//! di requirements i marker restano, e a valutarli è pip: il pezzo di software
//! che sa farlo per definizione. Noi smettiamo di scegliere.
//!
//! Effetto collaterale gradito: con la versione giusta, su Noble esiste la wheel
//! precompilata (`gevent-24.2.1-cp312-manylinux…`), quindi non si compila affatto
//! e `--no-build-isolation` resta inerte. Il workaround Cython<3 serve dove serve
//! davvero — Jammy, dove per gevent 21.8.0 la wheel non esiste.

use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VENV_SUBDIR: &str = "sandbox";
const REPO_SUBDIR: &str = "odoo";
/// Cache di pip, **dentro** il venv: sparisce con il `rm -rf sandbox` dell'undo
/// di `CreateVirtualenv` invece di restare in `/opt/odoo/.cache` (A-R5-3).
const PIP_CACHE_SUBDIR: &str = ".pip-cache";

/// Installa le dipendenze pip nel venv (senza undo proprio).
pub struct InstallPythonRequirements {
    ops: Box<dyn SystemOps>,
    installed: bool,
}

impl InstallPythonRequirements {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            installed: false,
        }
    }

    /// Scrive un file di requirements **dentro il venv**, non in `/tmp`, e lo
    /// consegna all'utente che dovrà leggerlo (A-V3-3).
    ///
    /// # Perché non `/tmp`
    ///
    /// Qui root scrive un file che pip legge come utente `odoo`: due operazioni
    /// distinte, con una finestra in mezzo. In `/tmp` — world-writable, e con un
    /// nome fisso scritto nel sorgente — chi controlla un utente locale
    /// qualsiasi può provare a sostituire il file in quella finestra e far
    /// installare a pip pacchetti arbitrari nel venv: esecuzione di codice come
    /// il proprietario del filestore e del database. Non è il caso del symlink
    /// (che `fs.protected_symlinks` mitiga): è la sostituzione del **contenuto**,
    /// e lì il kernel non aiuta.
    ///
    /// `<install_dir>/sandbox` toglie il presupposto invece di difendersi
    /// dall'attacco: è di proprietà di `odoo` e non è scrivibile da altri, quindi
    /// nessun terzo può creare o sostituire nulla al suo interno. In più il file
    /// nasce e muore dentro il perimetro reversibile — l'undo di
    /// `CreateVirtualenv` fa `rm -rf sandbox` — quindi un'esecuzione interrotta
    /// non lascia residui fuori.
    ///
    /// Nome imprevedibile e creazione fail-closed (`O_EXCL | O_NOFOLLOW`) restano
    /// comunque: sono la difesa che R1 ha scelto per il `.conf`, e questa è la
    /// stessa classe di problema. Il `chown` finale serve perché il file nasce
    /// `0600 root` e chi deve leggerlo è `odoo`.
    fn write_requirements_for_user(
        &self,
        venv: &std::path::Path,
        user: &str,
        nome: &str,
        content: &str,
    ) -> Result<std::path::PathBuf, StepError> {
        let path = crate::system_ops::private_temp_path(&venv.join(nome), nome);
        self.ops.create_private_file(&path, content)?;
        self.ops.chown_named(&path, user, user)?;
        Ok(path)
    }
}

impl Step for InstallPythonRequirements {
    fn name(&self) -> &str {
        "install-python-requirements"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // Snapshot leggero: lo stato rilevante per il rollback è quello del venv
        // (9b), non un PreState per-pacchetto. Nulla da rilevare qui.
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry-run): pip upgrade + Cython<3 + gevent (no-build-isolation) + requirements");
            return Ok(());
        }

        let user = &ctx.odoo_user;
        let venv = ctx.install_dir.join(VENV_SUBDIR);
        let pip = venv.join("bin").join("pip");
        let pip = pip.to_string_lossy();
        let requirements = ctx.install_dir.join(REPO_SUBDIR).join("requirements.txt");
        // Cache nel nostro perimetro: vedi la nota di modulo (A-R5-3).
        let cache_dir = venv.join(PIP_CACHE_SUBDIR);
        let cache_dir = cache_dir.to_string_lossy();

        // requirements.txt deve esistere (read_to_string fallisce se assente).
        let content = self.ops.read_to_string(&requirements)?;

        // 1) pip + wheel + setuptools aggiornati.
        //
        // `setuptools` non è decorativo ed è la ragione per cui questo step
        // falliva su Ubuntu 24.04 (A-R6-2): da Python 3.12 `venv` **non semina
        // più setuptools**, ma il passo 3 usa `--no-build-isolation`, cioè
        // chiede a pip di costruire gevent con gli strumenti presenti nel venv.
        // Senza setuptools lì dentro, il backend di build non esiste e pip
        // muore con `BackendUnavailable: Cannot import 'setuptools.build_meta'`.
        // Il `python3-setuptools` di sistema non c'entra: il venv è isolato.
        self.ops.run_as_user(
            user,
            &pip,
            &[
                "install",
                "--quiet",
                "--cache-dir",
                &cache_dir,
                "--upgrade",
                "pip",
                "wheel",
                "setuptools",
            ],
        )?;

        // 2) Cython compatibile (< 3.0).
        self.ops.run_as_user(
            user,
            &pip,
            &["install", "--quiet", "--cache-dir", &cache_dir, "Cython<3"],
        )?;

        // 3) gevent (+ greenlet) dalle righe di requirements **con i marker**,
        //    build senza isolamento. Il file, non argv: così i marker
        //    sopravvivono e a scegliere la versione è pip (A-R6-3).
        let gevent_lines = gevent_stack_lines(&content);
        if gevent_lines.trim().is_empty() {
            info!("run: nessuna riga gevent nei requirements, salto il passo dedicato");
        } else {
            let tmp_gevent = self.write_requirements_for_user(
                &venv,
                user,
                "requirements-gevent.txt",
                &gevent_lines,
            )?;
            let tmp_gevent_str = tmp_gevent.to_string_lossy().into_owned();
            let outcome = self.ops.run_as_user(
                user,
                &pip,
                &[
                    "install",
                    "--quiet",
                    "--cache-dir",
                    &cache_dir,
                    "--no-build-isolation",
                    "--requirement",
                    &tmp_gevent_str,
                ],
            );
            let _ = self.ops.remove_file(&tmp_gevent);
            outcome?;
        }

        // 4) resto dei requirements, escludendo ciò che il passo 3 ha già
        //    installato (gevent e greenlet).
        let filtered = filter_out_gevent_stack(&content);
        let tmp_req =
            self.write_requirements_for_user(&venv, user, "requirements-filtered.txt", &filtered)?;
        let tmp_str = tmp_req.to_string_lossy().into_owned();
        let outcome = self.ops.run_as_user(
            user,
            &pip,
            &[
                "install",
                "--quiet",
                "--cache-dir",
                &cache_dir,
                "--prefer-binary",
                "--requirement",
                &tmp_str,
            ],
        );
        let _ = self.ops.remove_file(&tmp_req);
        outcome?;

        self.installed = true;
        info!("run: dipendenze Python installate");
        Ok(())
    }

    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        // NO-OP deliberato: i pacchetti pip sono nel venv; la loro rimozione è
        // coperta dall'undo di CreateVirtualenv (rm -rf sandbox).
        info!(
            "undo NO-OP: i pacchetti pip vivono nel venv; la rimozione è coperta \
             dall'undo di CreateVirtualenv"
        );
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Bool(self.installed)
    }

    /// Reidratato per simmetria: l'`undo` è un NO-OP (la pulizia è del venv),
    /// ma il contratto `snapshot_value` ⇄ `rehydrate` vale per ogni step.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let installed = decode_snapshot(self.name(), snapshot)?;
        self.installed = installed;
        Ok(())
    }
}

/// I pacchetti che il passo 3 installa a parte, senza isolamento del build.
///
/// `greenlet` sta qui insieme a `gevent` perché è la sua controparte C: Odoo lo
/// pinna con gli stessi marker per versione di Python, e installare i due in
/// momenti diversi significherebbe lasciare che il risolutore di pip scelga un
/// greenlet qualunque compatibile con la *metadata* di gevent — che è come si è
/// arrivati a compilare `greenlet 1.1.x` contro Python 3.12 (A-R6-3).
const BUILD_ISOLATED_PACKAGES: [&str; 2] = ["gevent", "greenlet"];

/// Le righe di `gevent`/`greenlet` **verbatim**, marker d'ambiente inclusi.
///
/// Verbatim è il punto: i marker (`; python_version >= '3.12'`) sono l'unica
/// cosa che distingue la versione giusta da una che non compila, e valutarli non
/// è compito nostro. Il risultato va scritto in un file e passato a pip con
/// `--requirement`; pip tiene la riga applicabile e scarta le altre.
///
/// Stringa vuota se il `requirements.txt` non nomina nessuno dei due — nel qual
/// caso il passo dedicato non ha ragione di esistere e viene saltato.
pub fn gevent_stack_lines(requirements: &str) -> String {
    let selected: Vec<&str> = requirements
        .lines()
        .filter(|line| is_build_isolated_requirement(line))
        .collect();
    if selected.is_empty() {
        return String::new();
    }
    let mut out = selected.join("\n");
    out.push('\n');
    out
}

/// Il complemento di [`gevent_stack_lines`]: tutto il resto dei requirements.
pub fn filter_out_gevent_stack(requirements: &str) -> String {
    let mut out: String = requirements
        .lines()
        .filter(|line| !is_build_isolated_requirement(line))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// `true` se la riga è un requisito di uno dei [`BUILD_ISOLATED_PACKAGES`]: il
/// nome all'inizio, seguito da un confine (operatore, marker, spazio o fine),
/// case-insensitive. Il confine evita di catturare `gevent-websocket`.
fn is_build_isolated_requirement(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    BUILD_ISOLATED_PACKAGES.iter().any(|pkg| {
        lower
            .strip_prefix(pkg)
            .and_then(|rest| rest.chars().next())
            .map(|c| matches!(c, '>' | '=' | '<' | '!' | ';' | ' ' | '\t' | '#'))
            // `strip_prefix` con `rest` vuoto = la riga è esattamente il nome.
            .unwrap_or_else(|| lower == *pkg)
    })
}
