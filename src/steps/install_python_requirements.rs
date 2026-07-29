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

use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{RealSystemOps, SystemOps};

const VENV_SUBDIR: &str = "sandbox";
const REPO_SUBDIR: &str = "odoo";
/// Cache di pip, **dentro** il venv: sparisce con il `rm -rf sandbox` dell'undo
/// di `CreateVirtualenv` invece di restare in `/opt/odoo/.cache` (A-R5-3).
const PIP_CACHE_SUBDIR: &str = ".pip-cache";

/// Installa le dipendenze pip nel venv (senza undo proprio).
pub struct InstallPythonRequirements {
    ops: Box<dyn SystemOps>,
    /// Directory per il requirements temporaneo (configurabile nei test).
    tmp_dir: std::path::PathBuf,
    installed: bool,
}

impl InstallPythonRequirements {
    pub fn new() -> Self {
        Self::with_parts(Box::new(RealSystemOps::new()), std::env::temp_dir())
    }

    pub fn with_parts(ops: Box<dyn SystemOps>, tmp_dir: std::path::PathBuf) -> Self {
        Self {
            ops,
            tmp_dir,
            installed: false,
        }
    }
}

impl Default for InstallPythonRequirements {
    fn default() -> Self {
        Self::new()
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

        // 1) pip + wheel aggiornati.
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
            ],
        )?;

        // 2) Cython compatibile (< 3.0).
        self.ops.run_as_user(
            user,
            &pip,
            &["install", "--quiet", "--cache-dir", &cache_dir, "Cython<3"],
        )?;

        // 3) gevent con la spec da requirements, build senza isolamento.
        let gevent_spec = extract_gevent_spec(&content);
        self.ops.run_as_user(
            user,
            &pip,
            &[
                "install",
                "--quiet",
                "--cache-dir",
                &cache_dir,
                "--no-build-isolation",
                &gevent_spec,
            ],
        )?;

        // 4) resto dei requirements, escludendo gevent (già installato).
        let filtered = filter_out_gevent(&content);
        let tmp_req = self.tmp_dir.join("odoo-requirements-filtered.txt");
        std::fs::write(&tmp_req, filtered).map_err(|e| StepError::io(&tmp_req, e))?;
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
        let _ = std::fs::remove_file(&tmp_req);
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

/// Estrae la spec di gevent da `requirements.txt` (es. `"gevent==21.12.0"`),
/// rimuovendo marker d'ambiente (`;...`) e commenti (`#...`). Default `"gevent"`.
pub fn extract_gevent_spec(requirements: &str) -> String {
    for line in requirements.lines() {
        let line = line.trim();
        if !starts_with_gevent(line) {
            continue;
        }
        let spec = line.split([';', '#']).next().unwrap_or(line).trim();
        if !spec.is_empty() {
            return spec.to_string();
        }
    }
    "gevent".to_string()
}

/// Filtra via le righe che iniziano con `gevent` (già installato).
pub fn filter_out_gevent(requirements: &str) -> String {
    let mut out: String = requirements
        .lines()
        .filter(|line| !starts_with_gevent(line))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// `true` se la riga inizia con `gevent` seguito da un confine (operatore,
/// marker, spazio o fine), case-insensitive.
fn starts_with_gevent(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("gevent") else {
        return false;
    };
    rest.chars()
        .next()
        .map(|c| matches!(c, '>' | '=' | '<' | '!' | ';' | ' ' | '\t' | '#'))
        .unwrap_or(true)
}
