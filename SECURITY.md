# Security Policy

drawer sits between you and your servers, so security reports are taken seriously.

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Instead, report them privately through GitHub:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.

Include what you found, how to reproduce it, and the drawer version (`drawer --version`). You'll get a reply within a few days. Once a fix is released, you'll be credited unless you'd rather not be.

## Supported versions

Only the latest release gets security fixes.

## Scope

These are in scope:
- Anything that lets a remote machine write outside the folder you chose.
- Anything that lets it overwrite local files, run code on your computer, or tamper with your terminal.
- Anything that lets machine edits inject ssh options or corrupt your config.
- Anything that leaks secrets.

Bugs in OpenSSH itself should go to the OpenSSH project.
