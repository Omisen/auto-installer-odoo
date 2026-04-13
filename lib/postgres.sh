#!/usr/bin/env bash
# =============================================================================
# lib/postgres.sh — PostgreSQL setup for odoo-autoinstaller
#
# Public functions:
#   setup_postgres   — installa PostgreSQL se assente, abilita e avvia il servizio
#   create_db_user   — crea il ruolo PostgreSQL per Odoo (idempotente)
#   create_db_if_missing — crea il database applicativo se assente (idempotente)
#
# Variabili attese dall'ambiente (esportate da install.sh):
#   DB_USER     — nome del ruolo PostgreSQL da creare (default: odoo)
#   DB_PASSWORD — password del ruolo (vuoto = autenticazione peer/ident)
#
# Dipendenze esterne: log(), warn(), error()  — definite in install.sh
# Nessun altro modulo lib/ viene sourciato qui.
# =============================================================================

set -euo pipefail

# -----------------------------------------------------------------------------
# _postgres_is_installed()
#   Ritorna 0 se il binario postgres è disponibile, 1 altrimenti.
# -----------------------------------------------------------------------------
_postgres_is_installed() {
    command -v psql &>/dev/null
}

# -----------------------------------------------------------------------------
# _postgres_is_running()
#   Ritorna 0 se il servizio postgresql è attivo secondo systemctl, 1 altrimenti.
# -----------------------------------------------------------------------------
_postgres_is_running() {
    systemctl is-active --quiet postgresql
}

# -----------------------------------------------------------------------------
# _postgres_role_exists(role)
#   Ritorna 0 se il ruolo $1 esiste già in PostgreSQL, 1 altrimenti.
#   Eseguito come utente postgres tramite sudo.
# -----------------------------------------------------------------------------
_postgres_role_exists() {
    local role="${1:?_postgres_role_exists richiede un nome ruolo}"
    sudo -Hiu postgres -- psql -tAc \
        "SELECT 1 FROM pg_roles WHERE rolname = '${role}';" \
        2>/dev/null | grep -q 1
}

# -----------------------------------------------------------------------------
# _postgres_db_exists(db_name)
#   Ritorna 0 se il database $1 esiste già, 1 altrimenti.
# -----------------------------------------------------------------------------
_postgres_db_exists() {
    local db_name="${1:?_postgres_db_exists richiede un nome database}"
    sudo -Hiu postgres -- psql -tAc \
        "SELECT 1 FROM pg_database WHERE datname = '${db_name}';" \
        2>/dev/null | grep -q 1
}

# -----------------------------------------------------------------------------
# setup_postgres()
#   Installa PostgreSQL se non presente, poi assicura che il servizio sia
#   abilitato e in esecuzione.
#
#   Idempotente: se postgresql è già installato e attivo non fa nulla.
# -----------------------------------------------------------------------------
setup_postgres() {
    log "=== Setup PostgreSQL ==="

    # ── Installazione ──────────────────────────────────────────────────────
    if _postgres_is_installed; then
        log "PostgreSQL già installato — salto l'installazione."
    else
        log "Installazione PostgreSQL..."
        apt-get install -y postgresql postgresql-contrib
        log "PostgreSQL installato."
    fi

    # ── Abilitazione servizio ──────────────────────────────────────────────
    log "Abilitazione del servizio postgresql all'avvio..."
    systemctl enable postgresql

    # ── Avvio / restart ────────────────────────────────────────────────────
    if _postgres_is_running; then
        log "PostgreSQL è già in esecuzione."
    else
        log "Avvio del servizio postgresql..."
        systemctl start postgresql
    fi

    # ── Verifica finale ────────────────────────────────────────────────────
    if ! _postgres_is_running; then
        error "PostgreSQL non è riuscito ad avviarsi. Controlla: journalctl -u postgresql"
        return 1
    fi

    log "PostgreSQL attivo e funzionante."
}

# -----------------------------------------------------------------------------
# create_db_user()
#   Crea il ruolo PostgreSQL identificato da $DB_USER.
#
#   Comportamento:
#     - Se il ruolo esiste già, logga un avviso e non fa nulla (idempotente).
#     - Se $DB_PASSWORD è impostata e non vuota, imposta la password.
#     - Se $DB_PASSWORD è vuota o non impostata, crea il ruolo senza password
#       (autenticazione peer/ident, che è il default sicuro per installazioni
#       locali dove l'utente OS e il ruolo PG hanno lo stesso nome).
#
#   Variabili richieste:
#     DB_USER      — nome del ruolo (obbligatorio)
#     DB_PASSWORD  — password del ruolo (opzionale)
# -----------------------------------------------------------------------------
create_db_user() {
    log "=== Creazione utente PostgreSQL ==="

    # ── Validazione variabili ──────────────────────────────────────────────
    if [[ -z "${DB_USER:-}" ]]; then
        error "DB_USER non è impostata. Impossibile creare il ruolo PostgreSQL."
        return 1
    fi

    # ── Idempotenza: verifica ruolo esistente ──────────────────────────────
    if _postgres_role_exists "${DB_USER}"; then
        warn "Il ruolo PostgreSQL '${DB_USER}' esiste già — nessuna azione."
        return 0
    fi

    # ── Creazione ruolo ────────────────────────────────────────────────────
    if [[ -n "${DB_PASSWORD:-}" ]]; then
        log "Creazione ruolo '${DB_USER}' con password..."
        # Escape SQL minimo per password con apici singoli.
        local sql_password
        sql_password="${DB_PASSWORD//\'/\'\'}"
        sudo -Hiu postgres -- psql -c \
            "CREATE ROLE \"${DB_USER}\" WITH LOGIN CREATEDB PASSWORD '${sql_password}';"
    else
        log "Creazione ruolo '${DB_USER}' senza password (autenticazione peer)..."
        sudo -Hiu postgres -- psql -c \
            "CREATE ROLE \"${DB_USER}\" WITH LOGIN CREATEDB;"
    fi

    # ── Verifica post-creazione ────────────────────────────────────────────
    if _postgres_role_exists "${DB_USER}"; then
        log "Ruolo PostgreSQL '${DB_USER}' creato con successo."
    else
        error "Creazione del ruolo '${DB_USER}' fallita in modo inatteso."
        return 1
    fi
}

# -----------------------------------------------------------------------------
# create_db_if_missing()
#   Crea il database applicativo ($DB_NAME) se non esiste.
#   DB_NAME è obbligatorio: se vuota la funzione termina con errore.
# -----------------------------------------------------------------------------
create_db_if_missing() {
    log "=== Creazione database PostgreSQL ==="

    if [[ -z "${DB_NAME:-}" ]]; then
        error "DB_NAME è obbligatorio e non può essere vuoto."
        return 1
    fi

    if [[ -z "${DB_USER:-}" ]]; then
        error "DB_USER non è impostata. Impossibile creare il database '${DB_NAME}'."
        return 1
    fi

    if _postgres_db_exists "${DB_NAME}"; then
        warn "Il database PostgreSQL '${DB_NAME}' esiste già — nessuna azione."
        return 0
    fi

    log "Creazione database '${DB_NAME}' con owner '${DB_USER}'..."
    sudo -Hiu postgres -- createdb --owner "${DB_USER}" "${DB_NAME}"

    if _postgres_db_exists "${DB_NAME}"; then
        log "Database PostgreSQL '${DB_NAME}' creato con successo."
    else
        error "Creazione del database '${DB_NAME}' fallita in modo inatteso."
        return 1
    fi
}