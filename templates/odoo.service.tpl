[Unit]
Description=Odoo {{ODOO_VERSION}} - ERP & CRM
Documentation=https://www.odoo.com
Requires=postgresql.service
Wants=network-online.target
After=network.target network-online.target postgresql.service

# Restart burst protection (unit-level)
StartLimitIntervalSec=60
StartLimitBurst=3

[Service]
Type=simple
SyslogIdentifier=odoo{{ODOO_VERSION_SHORT}}

# ── Identità e isolamento ────────────────────────────────────────────────────
User={{ODOO_USER}}
Group={{ODOO_USER}}
WorkingDirectory={{ODOO_INSTALL_DIR}}
NoNewPrivileges=true
PrivateTmp=true
RuntimeDirectory=odoo
RuntimeDirectoryMode=0750

# ── Hardening (A-V3-13) ──────────────────────────────────────────────────────
# Prima qui c'era `PermissionsStartOnly=true`, deprecato da systemd 231 (2016) e
# ignorato con un warning: serviva a far girare gli ExecStartPre come root, e di
# ExecStartPre non ce n'è nessuno. Sotto un'intestazione "Security hardening"
# c'era quindi una direttiva inerte e nient'altro.
#
# ProtectSystem=full monta /usr e /boot in sola lettura; /opt — dove vive Odoo —
# resta scrivibile. NON si usa `strict`, che renderebbe l'intero filesystem in
# sola lettura e richiederebbe un elenco esatto di ReadWritePaths (install dir,
# filestore, cache, sessioni): sbagliarne uno rompe il servizio su una macchina
# cliente, e il guadagno rispetto a `full` è marginale per un processo che gira
# già senza privilegi.
ProtectSystem=full
ProtectHome=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
# AF_UNIX serve al socket di PostgreSQL, non solo la rete.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

# Binary & config
ExecStart={{ODOO_INSTALL_DIR}}/{{ODOO_VENV_DIR}}/bin/python3 \
    {{ODOO_INSTALL_DIR}}/{{ODOO_REPO_DIR}}/odoo-bin \
    -c {{ODOO_INSTALL_DIR}}/odoo{{ODOO_VERSION_SHORT}}.conf

StandardOutput=journal+console
StandardError=journal+console

# Restart policy
Restart=on-failure
RestartSec=5s

# Resource limits (tunable per environment)
LimitNOFILE=65536
LimitNPROC=65536

[Install]
WantedBy=multi-user.target
