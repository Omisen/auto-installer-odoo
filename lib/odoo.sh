#!/usr/bin/env bash
# =============================================================================
# lib/odoo.sh — Clone repo Odoo, virtualenv, pip install requirements
#
# Funzioni pubbliche:
#   install_odoo       — orchestratore principale del modulo
#
# Variabili attese dall'ambiente (esportate da install.sh):
#   ODOO_VERSION       — branch Git, es. "18.0"
#   ODOO_USER          — utente di sistema, es. "odoo"
#   ODOO_HOME          — home dell'utente, es. "/opt/odoo" 
#   ODOO_INSTALL_DIR   — cartella di destinazione, es. "/opt/odoo/odoo18" 
#   ODOO_REPO_DIR      — sottocartella del clone, es. "odoo"    (relativa a ODOO_INSTALL_DIR)
#   ODOO_MODULES_DIR   — addons extra, es. "repos/modules"      (relativa a ODOO_INSTALL_DIR)
#   ODOO_VENV_DIR      — virtualenv, es. "sandbox"              (relativa a ODOO_INSTALL_DIR)
#   GIT_DEPTH          — profondità clone, default 5
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Controlla che le variabili obbligatorie siano definite
# ---------------------------------------------------------------------------
_odoo_check_env() {
    local required_vars=(
        ODOO_VERSION
        ODOO_USER
        ODOO_HOME
        ODOO_INSTALL_DIR
        ODOO_REPO_DIR
        ODOO_MODULES_DIR
        ODOO_VENV_DIR
    )

    local missing=()
    for var in "${required_vars[@]}"; do
        [[ -z "${!var:-}" ]] && missing+=("$var")
    done

    if [[ ${#missing[@]} -gt 0 ]]; then
        error "odoo.sh — variabili mancanti: ${missing[*]}"
        return 1
    fi
}

# ---------------------------------------------------------------------------
# Crea le directory di lavoro come utente odoo
# ---------------------------------------------------------------------------
_create_directories() {
    log "Creazione struttura directory in ${ODOO_INSTALL_DIR} …"

    local dirs=(
        "${ODOO_INSTALL_DIR}"
        "${ODOO_INSTALL_DIR}/${ODOO_MODULES_DIR}"
    )

    for dir in "${dirs[@]}"; do
        if [[ -d "$dir" ]]; then
            warn "Directory già esistente, salto: ${dir}"
        else
            sudo -u "${ODOO_USER}" mkdir -p "$dir"
            log "  ✔ Creata: ${dir}"
        fi
    done
}

# ---------------------------------------------------------------------------
# _fetch_odoo_tarball_fallback <target_dir>
# Scarica il branch Odoo come tarball GitHub e lo estrae in target_dir.
# Utile quando il clone git fallisce per errori TLS/RPC intermittenti.
# ---------------------------------------------------------------------------
_fetch_odoo_tarball_fallback() {
    local target_dir="$1"
    local tar_url="https://codeload.github.com/odoo/odoo/tar.gz/refs/heads/${ODOO_VERSION}"
    local tmp_tar

    tmp_tar="$(mktemp /tmp/odoo-src-XXXXXX.tar.gz)"

    log "Tentativo fallback: download tarball Odoo ${ODOO_VERSION} …"
    log "  URL: ${tar_url}"

    if command -v curl &>/dev/null; then
        curl --fail --location --silent --show-error "${tar_url}" -o "${tmp_tar}"
    elif command -v wget &>/dev/null; then
        wget -qO "${tmp_tar}" "${tar_url}"
    else
        rm -f "${tmp_tar}"
        error "Né curl né wget disponibili: impossibile usare il fallback tarball."
        return 1
    fi

    sudo mkdir -p "${target_dir}"
    sudo tar -xzf "${tmp_tar}" -C "${target_dir}" --strip-components=1
    sudo chown -R "${ODOO_USER}:${ODOO_USER}" "${target_dir}"
    rm -f "${tmp_tar}"

    if [[ -f "${target_dir}/odoo-bin" ]]; then
        log "  ✔ Sorgenti Odoo installate via tarball fallback."
        return 0
    fi

    error "Fallback tarball completato ma odoo-bin non trovato in ${target_dir}."
    return 1
}

# ---------------------------------------------------------------------------
# Clone del repository Odoo da GitHub
# Idempotente: se la cartella esiste e ha già il branch corretto, salta.
# ---------------------------------------------------------------------------
_clone_odoo_repo() {
    local target_dir="${ODOO_INSTALL_DIR}/${ODOO_REPO_DIR}"
    local depth="${GIT_DEPTH:-5}"
    local retries="${GIT_CLONE_RETRIES:-3}"
    local attempt

    if [[ -d "${target_dir}/.git" ]]; then
        # Verifica che il branch corrisponda alla versione attesa
        local current_branch
        current_branch=$(sudo -u "${ODOO_USER}" git -C "${target_dir}" \
                         rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")

        if [[ "$current_branch" == "${ODOO_VERSION}" ]]; then
            warn "Repository già clonato su branch '${ODOO_VERSION}', salto il clone."
            return 0
        else
            warn "Repository esistente su branch '${current_branch}', atteso '${ODOO_VERSION}'."
            warn "Rimuovi manualmente ${target_dir} se vuoi ri-clonare."
            error "Branch mismatch — installazione interrotta."
            return 1
        fi
    fi

    # Supporta installazioni già popolate da tarball fallback (senza .git).
    if [[ -d "${target_dir}" && ! -d "${target_dir}/.git" ]]; then
        if [[ -f "${target_dir}/odoo-bin" ]]; then
            warn "Sorgenti già presenti in ${target_dir} (senza metadata git), salto clone."
            return 0
        fi
        warn "Directory ${target_dir} esistente ma non valida: la rigenero."
        sudo rm -rf "${target_dir}"
    fi

    log "Clone Odoo ${ODOO_VERSION} da GitHub (depth=${depth}) …"
    log "  Destinazione: ${target_dir}"

    for ((attempt=1; attempt<=retries; attempt++)); do
        if sudo -u "${ODOO_USER}" git \
            -c http.version=HTTP/1.1 \
            -c core.compression=0 \
            clone \
            https://github.com/odoo/odoo.git \
            --branch "${ODOO_VERSION}" \
            --single-branch \
            --no-tags \
            --depth "${depth}" \
            "${target_dir}"; then
            log "  ✔ Clone completato."
            return 0
        fi

        warn "Clone Odoo fallito (tentativo ${attempt}/${retries})."

        # Pulizia di eventuali artefatti parziali prima del retry.
        if [[ -e "${target_dir}" ]]; then
            sudo rm -rf "${target_dir}"
        fi

        if [[ "${attempt}" -lt "${retries}" ]]; then
            local backoff=$((attempt * 2))
            warn "Nuovo tentativo tra ${backoff}s..."
            sleep "${backoff}"
        fi
    done

    warn "Clone Odoo fallito dopo ${retries} tentativi. Attivo fallback tarball..."
    _fetch_odoo_tarball_fallback "${target_dir}"
}

# ---------------------------------------------------------------------------
# Crea il virtual environment Python
# Idempotente: se la sandbox esiste e ha un Python funzionante, salta.
# ---------------------------------------------------------------------------
_create_virtualenv() {
    local venv_dir="${ODOO_INSTALL_DIR}/${ODOO_VENV_DIR}"

    if [[ -x "${venv_dir}/bin/python3" ]]; then
        warn "Virtualenv già presente in ${venv_dir}, salto la creazione."
        return 0
    fi

    log "Creazione virtualenv Python in ${venv_dir} …"

    # Verifica che python3-venv sia disponibile
    if ! python3 -m venv --help &>/dev/null; then
        error "python3-venv non disponibile. Installare prima le dipendenze di sistema."
        return 1
    fi

    sudo -u "${ODOO_USER}" python3 -m venv "${venv_dir}"
    log "  ✔ Virtualenv creato."
}

# ---------------------------------------------------------------------------
# Installa le dipendenze Python tramite pip
# Idempotente a grandi linee: pip salta i pacchetti già installati.
# ---------------------------------------------------------------------------
_install_python_requirements() {
    local venv_dir="${ODOO_INSTALL_DIR}/${ODOO_VENV_DIR}"
    local pip="${venv_dir}/bin/pip"
    local requirements="${ODOO_INSTALL_DIR}/${ODOO_REPO_DIR}/requirements.txt"

    if [[ ! -f "$requirements" ]]; then
        error "File requirements.txt non trovato: ${requirements}"
        return 1
    fi

    log "Aggiornamento pip/wheel nel virtualenv …"
    sudo -u "${ODOO_USER}" "${pip}" install --quiet --upgrade pip wheel

    # Cython 3.x ha rimosso il tipo 'long' di Python 2; gevent (richiesto da
    # Odoo 18) usa ancora quel codice Cython nei file .pyx.
    # pip costruisce le wheel in un ambiente isolato (/tmp/pip-build-env-*/),
    # ignorando il Cython del virtualenv. Soluzione:
    #   1. Installare Cython<3 nel virtualenv
    #   2. Installare gevent con --no-build-isolation, che usa il Cython locale
    #   3. Installare il resto dei requirements normalmente (gevent già presente)
    log "Installazione Cython compatibile (< 3.0) …"
    sudo -u "${ODOO_USER}" "${pip}" install --quiet "Cython<3"

    # Estrae la specifica esatta di gevent dal requirements.txt (es. "gevent==21.12.0")
    # così la versione pre-installata combacia con quella attesa da Odoo.
    # I marker di ambiente ("; sys_platform...") e i commenti ("# ...") vengono
    # rimossi: --no-build-isolation non li supporta e causano ParserSyntaxError.
    local gevent_req
    gevent_req=$(grep -iE '^gevent([>=<!;[:space:]]|$)' "${requirements}" \
                 | head -1 \
                 | sed 's/[;#].*//' \
                 | sed 's/[[:space:]]*$//')
    gevent_req="${gevent_req:-gevent}"

    log "Pre-installazione gevent (build senza isolamento) …"
    sudo -u "${ODOO_USER}" "${pip}" install --quiet --no-build-isolation "${gevent_req}"

    # Genera un requirements temporaneo senza le righe gevent: il pacchetto è
    # già installato e pip altrimenti lo rischerica e ricompila da sorgente
    # nell'env isolato (/tmp/pip-build-env-*/), ignorando il Cython<3 del venv.
    local tmp_req
    tmp_req=$(mktemp /tmp/odoo-requirements-XXXXXX.txt)
    chmod 644 "${tmp_req}"
    grep -iv '^gevent' "${requirements}" > "${tmp_req}"

    log "Installazione dipendenze Python da requirements.txt …"
    log "  (Questo passaggio può richiedere qualche minuto)"

    sudo -u "${ODOO_USER}" "${pip}" install \
        --quiet \
        --prefer-binary \
        --requirement "${tmp_req}"

    rm -f "${tmp_req}"

    log "  ✔ Dipendenze Python installate."
}

# ---------------------------------------------------------------------------
# Verifica veloce post-installazione
# ---------------------------------------------------------------------------
_verify_installation() {
    local odoo_bin="${ODOO_INSTALL_DIR}/${ODOO_REPO_DIR}/odoo-bin"
    local python="${ODOO_INSTALL_DIR}/${ODOO_VENV_DIR}/bin/python3"

    log "Verifica installazione Odoo …"

    if [[ ! -f "$odoo_bin" ]]; then
        error "odoo-bin non trovato in ${odoo_bin}"
        return 1
    fi

    if [[ ! -x "$python" ]]; then
        error "Python del virtualenv non trovato o non eseguibile: ${python}"
        return 1
    fi

    # Prova ad importare odoo aggiungendo la directory del repo al PYTHONPATH.
    # Odoo non è installato come package pip ma è un clone git: il modulo 'odoo'
    # si trova in ODOO_INSTALL_DIR/ODOO_REPO_DIR/ e va reso visibile a Python.
    local repo_dir="${ODOO_INSTALL_DIR}/${ODOO_REPO_DIR}"
    if ! sudo -u "${ODOO_USER}" \
            PYTHONPATH="${repo_dir}" \
            "${python}" -c "import odoo" &>/dev/null; then
        error "Impossibile importare il modulo 'odoo'. Controllare l'installazione pip."
        return 1
    fi

    log "  ✔ odoo-bin trovato: ${odoo_bin}"
    log "  ✔ Python virtualenv: ${python}"
    log "  ✔ Modulo odoo importabile."
}

# =============================================================================
# FUNZIONE PUBBLICA
# =============================================================================

install_odoo() {
    log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    log "  Installazione Odoo ${ODOO_VERSION}"
    log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    _odoo_check_env
    _create_directories
    _clone_odoo_repo
    _create_virtualenv
    _install_python_requirements
    _verify_installation

    log "  ✅ Odoo ${ODOO_VERSION} installato con successo."
    log ""
    log "  Struttura:"
    log "    repo    → ${ODOO_INSTALL_DIR}/${ODOO_REPO_DIR}"
    log "    modules → ${ODOO_INSTALL_DIR}/${ODOO_MODULES_DIR}"
    log "    venv    → ${ODOO_INSTALL_DIR}/${ODOO_VENV_DIR}"
}