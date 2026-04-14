#!/usr/bin/env bash
# =============================================================================
# lib/systemd.sh — Systemd service setup for odoo-autoinstaller
#
# Public functions:
#   setup_systemd   — renders template, installs unit, enables & starts service
#   systemd_status  — prints current status of the Odoo service unit
#
# Depends on globals exported by install.sh:
#   ODOO_VERSION, ODOO_USER, ODOO_HOME, TEMPLATES_DIR
#
# Conventions:
#   - set -euo pipefail inherited from caller (install.sh)
#   - Logging via log(), warn(), error() defined in install.sh
#   - Template placeholders: {{VARIABLE}}
#   - Idempotent: safe to re-run (overwrites existing unit file)
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

# Derive the short version tag used in paths (e.g. "18.0" → "18")
_odoo_version_short() {
    echo "${ODOO_VERSION%%.*}"
}

# Name of the systemd unit (e.g. "odoo18")
_unit_name() {
    echo "odoo$(_odoo_version_short)"
}

# Full path of the installed unit file
_unit_file() {
    echo "/etc/systemd/system/$(_unit_name).service"
}

# Render {{PLACEHOLDER}} tokens in a template using sed.
# Usage: _render_template <src_tpl> <dst_file>
_render_template() {
    local src="$1"
    local dst="$2"

    local version_short
    version_short="$(_odoo_version_short)"

    sed \
        -e "s|{{ODOO_VERSION}}|${ODOO_VERSION}|g" \
        -e "s|{{ODOO_VERSION_SHORT}}|${version_short}|g" \
        -e "s|{{ODOO_USER}}|${ODOO_USER}|g" \
        -e "s|{{ODOO_HOME}}|${ODOO_HOME}|g" \
        -e "s|{{ODOO_INSTALL_DIR}}|${ODOO_INSTALL_DIR}|g" \
        -e "s|{{ODOO_REPO_DIR}}|${ODOO_REPO_DIR}|g" \
        -e "s|{{ODOO_VENV_DIR}}|${ODOO_VENV_DIR}|g" \
        "${src}" > "${dst}"
}

# ---------------------------------------------------------------------------
# _validate_template
#   Checks the rendered unit file for common mistakes before installing.
# ---------------------------------------------------------------------------
_validate_template() {
    local unit_file="$1"

    # Ensure no unreplaced placeholders remain
    if grep -qE '\{\{[A-Z_]+\}\}' "${unit_file}"; then
        local leftover
        leftover="$(grep -oE '\{\{[A-Z_]+\}\}' "${unit_file}" | sort -u | tr '\n' ' ')"
        error "Unreplaced placeholders in unit file: ${leftover}"
        return 1
    fi

    # Ensure ExecStart binary actually exists
    local execstart_bin
    execstart_bin=$(awk '/ExecStart=/{print $1}' "${unit_file}" \
                    | sed 's/ExecStart=//')
    if [[ ! -x "${execstart_bin}" ]]; then
        warn "ExecStart binary not found or not executable: ${execstart_bin}"
        warn "Continuing — binary may be installed in a later step."
    fi

    # Verifica sintattica systemd quando disponibile.
    if command -v systemd-analyze &>/dev/null; then
        if ! systemd-analyze verify "${unit_file}" >/dev/null 2>&1; then
            warn "systemd-analyze verify ha segnalato problemi sulla unit renderizzata."
            systemd-analyze verify "${unit_file}" || true
        fi
    fi

    return 0
}

# ---------------------------------------------------------------------------
# _install_unit_file
#   Copies the rendered unit to /etc/systemd/system/ and reloads daemon.
# ---------------------------------------------------------------------------
_install_unit_file() {
    local rendered_unit="$1"
    local dest
    dest="$(_unit_file)"

    log "Installing systemd unit → ${dest}"
    sudo cp "${rendered_unit}" "${dest}"
    sudo chmod 644 "${dest}"
    sudo chown root:root "${dest}"

    log "Reloading systemd daemon..."
    sudo systemctl daemon-reload
}

# ---------------------------------------------------------------------------
# _enable_service
#   Enables the unit so it starts at boot (idempotent).
# ---------------------------------------------------------------------------
_enable_service() {
    local unit
    unit="$(_unit_name)"

    if systemctl is-enabled --quiet "${unit}" 2>/dev/null; then
        log "Service '${unit}' is already enabled — skipping."
    else
        log "Enabling service '${unit}' at boot..."
        sudo systemctl enable "${unit}"
    fi
}

# ---------------------------------------------------------------------------
# _start_service
#   Starts (or restarts) the Odoo service and waits briefly for it to settle.
# ---------------------------------------------------------------------------
_start_service() {
    local unit
    unit="$(_unit_name)"

    if systemctl is-active --quiet "${unit}" 2>/dev/null; then
        log "Service '${unit}' is running — restarting to apply new config..."
        sudo systemctl restart "${unit}"
    else
        log "Starting service '${unit}'..."
        sudo systemctl start "${unit}"
    fi

    # Give systemd 3 s to propagate the state change
    sleep 3

    if systemctl is-active --quiet "${unit}"; then
        log "✅  Service '${unit}' started successfully."
    else
        warn "Service '${unit}' failed to start. Check logs with:"
        warn "  journalctl -u ${unit} -n 50 --no-pager"
        warn "Stato corrente unit '${unit}':"
        sudo systemctl --no-pager --full status "${unit}" || true
        warn "Ultime 50 righe journal per '${unit}':"
        sudo journalctl -u "${unit}" -n 50 --no-pager || true
        error "Avvio servizio '${unit}' fallito."
    fi
}

# ---------------------------------------------------------------------------
# Public: setup_systemd
#   Orchestrates template rendering → install → enable → start.
# ---------------------------------------------------------------------------
setup_systemd() {
    log "=== Setting up systemd service ==="

    # ── 1. Locate template ──────────────────────────────────────────────────
    local tpl="${TEMPLATES_DIR}/odoo.service.tpl"
    if [[ ! -f "${tpl}" ]]; then
        error "Template not found: ${tpl}"
        return 1
    fi

    # ── 2. Render into a temp file ──────────────────────────────────────────
    local tmp_unit
    tmp_unit="$(mktemp /tmp/odoo.XXXXXX.service)"
    # shellcheck disable=SC2064
    trap "rm -f '${tmp_unit}'" RETURN

    log "Rendering template: ${tpl}"
    _render_template "${tpl}" "${tmp_unit}"

    # ── 3. Validate rendered unit ───────────────────────────────────────────
    log "Validating rendered unit file..."
    _validate_template "${tmp_unit}"

    # ── 4. Install, enable, start ───────────────────────────────────────────
    _install_unit_file  "${tmp_unit}"
    _enable_service
    _start_service

    log "=== Systemd setup complete ==="
}

# ---------------------------------------------------------------------------
# Public: systemd_status
#   Prints a brief human-readable status for the Odoo unit.
# ---------------------------------------------------------------------------
systemd_status() {
    local unit
    unit="$(_unit_name)"

    echo ""
    echo "──────────────────────────────────────────"
    echo "  Systemd unit : ${unit}.service"
    echo "  Enabled      : $(systemctl is-enabled "${unit}" 2>/dev/null || echo 'unknown')"
    echo "  Active       : $(systemctl is-active  "${unit}" 2>/dev/null || echo 'unknown')"
    echo "──────────────────────────────────────────"
    echo ""
    systemctl --no-pager status "${unit}" 2>/dev/null || true
}