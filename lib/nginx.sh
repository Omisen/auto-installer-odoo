#!/usr/bin/env bash
# =============================================================================
# lib/nginx.sh — Reverse proxy Nginx per Odoo 18
# =============================================================================
# Funzioni pubbliche:
#   setup_nginx        — entry point principale, orchestratore del modulo
#
# Funzioni private (prefisso _nginx_):
#   _nginx_install         — installa Nginx se assente
#   _nginx_write_config    — genera il vhost da template
#   _nginx_enable_site     — symlink in sites-enabled + rimozione default
#   _nginx_open_firewall   — apre porte 80/443 con ufw (se disponibile)
#   _nginx_validate        — nginx -t
#   _nginx_reload          — reload sicuro del demone
#
# Variabili attese dall'ambiente (esportate da install.sh):
#   ODOO_PORT        porta locale su cui ascolta Odoo (default 8069)
#   ODOO_USER        nome utente di sistema per Odoo
#   TEMPLATES_DIR    percorso della directory templates/
#   WITH_NGINX       flag "true"/"false" — controllato da install.sh prima
#                    di chiamare setup_nginx, ma ricontrollato qui per sicurezza
#
# Variabili opzionali (possono essere impostate da .env o args):
#   NGINX_SERVER_NAME   nome di dominio/IP per il vhost (default: _)
#   NGINX_ENABLE_SSL    "true" per aggiungere redirect HTTP→HTTPS (default: false)
#   NGINX_CERT_PATH     percorso certificato TLS (necessario se SSL=true)
#   NGINX_KEY_PATH      percorso chiave privata TLS  (necessario se SSL=true)
#   NGINX_WORKER_PROCS  worker_processes (default: auto)
#   NGINX_CLIENT_MAX    client_max_body_size (default: 100m)
# =============================================================================

set -euo pipefail

# -----------------------------------------------------------------------------
# Costanti interne
# -----------------------------------------------------------------------------
readonly _NGINX_SITE_NAME="odoo${ODOO_VERSION%%.*}"
readonly _NGINX_SITES_AVAILABLE="/etc/nginx/sites-available"
readonly _NGINX_SITES_ENABLED="/etc/nginx/sites-enabled"
readonly _NGINX_DEFAULT_SITE="${_NGINX_SITES_ENABLED}/default"

# -----------------------------------------------------------------------------
# _nginx_install
# Installa Nginx tramite apt se il binario non è già presente.
# Idempotente: esce senza errori se Nginx è già installato.
# -----------------------------------------------------------------------------
_nginx_install() {
    if command -v nginx &>/dev/null; then
        log "Nginx già installato ($(nginx -v 2>&1 | head -1)) — skip."
        return 0
    fi

    log "Installazione Nginx..."
    apt-get update -qq
    apt-get install -y --no-install-recommends nginx
    systemctl enable nginx
    log "Nginx installato."
}

# -----------------------------------------------------------------------------
# _nginx_write_config
# Espande il template templates/nginx.conf.tpl sostituendo i placeholder
# {{VARIABILE}} con i valori correnti dell'ambiente, poi scrive il vhost in
# /etc/nginx/sites-available/odoo18.
#
# Dipende da: TEMPLATES_DIR, ODOO_PORT, NGINX_SERVER_NAME, NGINX_ENABLE_SSL,
#             NGINX_CERT_PATH, NGINX_KEY_PATH, NGINX_CLIENT_MAX
# -----------------------------------------------------------------------------
_nginx_write_config() {
    local tpl="${TEMPLATES_DIR}/nginx.conf.tpl"
    local dest="${_NGINX_SITES_AVAILABLE}/${_NGINX_SITE_NAME}"

    # Valori con default sicuri
    local server_name="${NGINX_SERVER_NAME:-_}"
    local odoo_port="${ODOO_PORT:-8069}"
    local enable_ssl="${NGINX_ENABLE_SSL:-false}"
    local cert_path="${NGINX_CERT_PATH:-/etc/ssl/certs/odoo.crt}"
    local key_path="${NGINX_KEY_PATH:-/etc/ssl/private/odoo.key}"
    local client_max="${NGINX_CLIENT_MAX:-100m}"
    local worker_procs="${NGINX_WORKER_PROCS:-auto}"

    if [[ ! -f "${tpl}" ]]; then
        error "Template non trovato: ${tpl}"
        error "Assicurati che templates/nginx.conf.tpl esista nel repository."
        return 1
    fi

    log "Generazione configurazione Nginx da template..."

    # envsubst richiederebbe di esportare variabili con nomi specifici;
    # usiamo sed per mantenere la coerenza con il pattern {{VARIABILE}} del progetto.
    sed \
        -e "s|{{NGINX_SERVER_NAME}}|${server_name}|g"   \
        -e "s|{{ODOO_PORT}}|${odoo_port}|g"             \
        -e "s|{{NGINX_ENABLE_SSL}}|${enable_ssl}|g"     \
        -e "s|{{NGINX_CERT_PATH}}|${cert_path}|g"       \
        -e "s|{{NGINX_KEY_PATH}}|${key_path}|g"         \
        -e "s|{{NGINX_CLIENT_MAX}}|${client_max}|g"     \
        -e "s|{{NGINX_WORKER_PROCS}}|${worker_procs}|g" \
        "${tpl}" > "${dest}"

    log "Vhost scritto in: ${dest}"
}

# -----------------------------------------------------------------------------
# _nginx_enable_site
# Crea il symlink in sites-enabled e rimuove il vhost "default" di Nginx
# per evitare conflitti sulla porta 80.
# Idempotente: ricrea il symlink se già esistente.
# -----------------------------------------------------------------------------
_nginx_enable_site() {
    local src="${_NGINX_SITES_AVAILABLE}/${_NGINX_SITE_NAME}"
    local lnk="${_NGINX_SITES_ENABLED}/${_NGINX_SITE_NAME}"

    # Rimozione vhost default (ignora se già assente)
    if [[ -f "${_NGINX_DEFAULT_SITE}" || -L "${_NGINX_DEFAULT_SITE}" ]]; then
        rm -f "${_NGINX_DEFAULT_SITE}"
        log "Vhost default Nginx rimosso."
    fi

    # Symlink idempotente
    ln -sf "${src}" "${lnk}"
    log "Sito abilitato: ${lnk} → ${src}"
}

# -----------------------------------------------------------------------------
# _nginx_open_firewall
# Apre le porte 80 (e 443 se SSL attivo) tramite ufw.
# Non interrompe l'installazione se ufw non è disponibile o non è attivo.
# -----------------------------------------------------------------------------
_nginx_open_firewall() {
    local enable_ssl="${NGINX_ENABLE_SSL:-false}"

    if ! command -v ufw &>/dev/null; then
        warn "ufw non trovato — skip apertura firewall. Apri manualmente le porte 80/443."
        return 0
    fi

    if ! ufw status | grep -q "^Status: active"; then
        warn "ufw presente ma non attivo — skip apertura firewall."
        return 0
    fi

    log "Apertura porta 80 (HTTP) su ufw..."
    ufw allow 80/tcp

    if [[ "${enable_ssl}" == "true" ]]; then
        log "Apertura porta 443 (HTTPS) su ufw..."
        ufw allow 443/tcp
    fi

    log "Regole firewall aggiornate."
}

# -----------------------------------------------------------------------------
# _nginx_validate
# Esegue "nginx -t" per verificare la correttezza sintattica della config.
# In caso di errore interrompe lo script (set -e).
# -----------------------------------------------------------------------------
_nginx_validate() {
    log "Validazione configurazione Nginx..."
    if ! nginx -t 2>&1 | tee /dev/stderr; then
        error "nginx -t ha riportato errori. Controlla la configurazione in:"
        error "  ${_NGINX_SITES_AVAILABLE}/${_NGINX_SITE_NAME}"
        return 1
    fi
    log "Configurazione Nginx valida."
}

# -----------------------------------------------------------------------------
# _nginx_reload
# Esegue un reload del demone Nginx in modo sicuro:
#   - se il servizio è già attivo  → systemctl reload
#   - se il servizio non è attivo  → systemctl start
# Questo evita di interrompere eventuali connessioni esistenti in ambiente
# di aggiornamento (idempotenza).
# -----------------------------------------------------------------------------
_nginx_reload() {
    if systemctl is-active --quiet nginx; then
        log "Reload Nginx (senza downtime)..."
        systemctl reload nginx
    else
        log "Avvio Nginx..."
        systemctl start nginx
    fi
    log "Nginx operativo."
}

# =============================================================================
# setup_nginx — funzione pubblica, entry point del modulo
# =============================================================================
# Orchestratore: chiama in sequenza le funzioni private.
# Se WITH_NGINX non è "true" la funzione ritorna immediatamente senza fare
# nulla, in modo che install.sh possa chiamarla incondizionatamente.
# =============================================================================
setup_nginx() {
    # Rispetto del flag globale impostato da install.sh / parse_args
    if [[ "${WITH_NGINX:-false}" != "true" ]]; then
        log "Setup Nginx saltato (--with-nginx non specificato)."
        return 0
    fi

    log "========================================"
    log " Configurazione reverse proxy Nginx"
    log "========================================"

    _nginx_install
    _nginx_write_config
    _nginx_enable_site
    _nginx_open_firewall
    _nginx_validate
    _nginx_reload

    log "----------------------------------------"
    log " Nginx configurato con successo."
    log " Dominio/IP : ${NGINX_SERVER_NAME:-_  (catch-all)}"
    log " Backend    : http://127.0.0.1:${ODOO_PORT:-8069}"
    if [[ "${NGINX_ENABLE_SSL:-false}" == "true" ]]; then
        log " TLS        : abilitato"
        log " Cert       : ${NGINX_CERT_PATH:-/etc/ssl/certs/odoo.crt}"
    else
        log " TLS        : disabilitato (aggiungi --enable-ssl per attivarlo)"
    fi
    log "----------------------------------------"
}