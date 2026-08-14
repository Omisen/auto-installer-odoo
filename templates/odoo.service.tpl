[Unit]
Description=Odoo {{ODOO_VERSION}} - ERP & CRM
Documentation=https://www.odoo.com
Requires=postgresql.service
Wants=network-online.target
After=network.target network-online.target postgresql.service

# restart burst protection (unit level)
StartLimitIntervalSec=60
StartLimitBurst=3

[Service]
Type=simple
SyslogIdentifier=odoo{{ODOO_VERSION_SHORT}}

# -- identity and isolation ---------------------------------------------------
User={{ODOO_USER}}
Group={{ODOO_USER}}
WorkingDirectory={{ODOO_INSTALL_DIR}}
NoNewPrivileges=true
PrivateTmp=true
RuntimeDirectory=odoo
RuntimeDirectoryMode=0750

# -- hardening (A-V3-13) ------------------------------------------------------
# this used to be `PermissionsStartOnly=true`, deprecated since systemd 231 and
# ignored with a warning: it made ExecStartPre run as root, and there is no
# ExecStartPre. under a "security hardening" heading sat an inert directive and
# nothing else.
#
# `ProtectSystem=full` mounts /usr and /boot read-only; /opt, where Odoo lives,
# stays writable. NOT `strict`, which would need an exact ReadWritePaths list
# (install dir, filestore, cache, sessions): getting one wrong breaks the service
# on a customer machine, for a marginal gain on a process that already runs
# unprivileged.
ProtectSystem=full
ProtectHome=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
# AF_UNIX is the PostgreSQL socket, not just the network.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

# -- binary and config --------------------------------------------------------
ExecStart={{ODOO_INSTALL_DIR}}/{{ODOO_VENV_DIR}}/bin/python3 \
    {{ODOO_INSTALL_DIR}}/{{ODOO_REPO_DIR}}/odoo-bin \
    -c {{ODOO_INSTALL_DIR}}/odoo{{ODOO_VERSION_SHORT}}.conf

StandardOutput=journal+console
StandardError=journal+console

# -- restart policy -----------------------------------------------------------
Restart=on-failure
RestartSec=5s

# -- resource limits, tunable per environment ---------------------------------
LimitNOFILE=65536
LimitNPROC=65536

[Install]
WantedBy=multi-user.target
