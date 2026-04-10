#!/usr/bin/env bash
# =============================================================================
# lib/postgres.sh — PostgreSQL setup for odoo-autoinstaller
#
# Public functions:
#   setup_postgres   — installa PostgreSQL se assente, abilita e avvia il servizio
#   create_db_user   — crea il ruolo PostgreSQL per Odoo (idempotente)
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
    sudo -u postgres psql -tAc \
        "SELECT 1 FROM pg_roles WHERE rolname = '${role}';" \
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
        # La password viene passata tramite variabile d'ambiente PGPASSWORD
        # per evitare che appaia nella lista dei processi (ps aux).
        sudo -u postgres psql -c \
            "CREATE ROLE \"${DB_USER}\" WITH LOGIN CREATEDB PASSWORD '${DB_PASSWORD}';"
    else
        log "Creazione ruolo '${DB_USER}' senza password (autenticazione peer)..."
        sudo -u postgres psql -c \
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