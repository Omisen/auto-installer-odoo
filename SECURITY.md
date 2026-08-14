# Security Policy

## Supported versions

This project is maintained on a rolling basis. Security fixes are applied to the repository's default branch.

| Version | Supported |
| --- | --- |
| Current default branch | Yes |
| Snapshots or forks that have diverged | No |
| Locally modified versions | No |

## Scope

Security reports relevant to this repository include, for example:

- unsafe handling of credentials or secrets in `.env` files, templates or logs;
- wrong permissions on configuration files, service files or directories the installer creates;
- injection through CLI input or configuration variables;
- generated configurations that expose Odoo, PostgreSQL or Nginx in unintended ways;
- unsafe use of `sudo`, systemd, shell expansion or temporary files.

## How to report a vulnerability

Do not open public issues containing sensitive details, proofs of concept or real credentials.

If the repository has the Security tab enabled, use `Report a vulnerability` on GitHub to send a private report.

If private reporting is unavailable, contact the maintainer over a non-public channel and share only the information strictly needed to reproduce the issue.

## What to include in a report

To speed up the analysis, include:

- a description of the problem and its expected impact;
- the minimum steps to reproduce it;
- the operating system version and the installed Odoo version;
- the file or module involved, if known;
- any temporary mitigations you have already verified.

## Handling process

The goal is to:

1. acknowledge receipt of the report;
2. reproduce and classify the problem;
3. prepare a fix or a mitigation;
4. coordinate disclosure once the fix is available.

Actual timelines depend on the complexity of the problem and on the maintainer's availability.

## Good practice for whoever uses the installer

- use strong passwords and do not leave the defaults in production environments;
- protect the `.env` files and the generated files containing secrets; the `.env` file is **parsed declaratively** (`KEY=VALUE`, no code execution), so it is not a code-execution vector;
- run the installer only on trusted, up-to-date hosts;
- limit the exposure of ports and services with a correctly configured firewall and reverse proxy;
- always check the final permissions of the configurations, the logs and the systemd unit.
