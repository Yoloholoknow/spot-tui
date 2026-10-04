# Security policy

spot-tui stores Spotify login tokens on your machine, so security reports are
welcome.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Use GitHub's
private reporting instead: open this repository's **Security** tab and choose
**Report a vulnerability**. Include what you found, how to reproduce it, and
what an attacker could do with it.

This is a small project maintained by one person, so replies are best effort,
usually within a week or two.

## What is in scope

- How spot-tui stores and handles the Spotify logins (`spotify_token.json` and
  librespot's `credentials.json` in the cache directory), including file
  permissions.
- The local sign-in callbacks on `127.0.0.1` ports 8888 and 8898.
- Secrets leaking into logs, such as API keys or tokens.

## What is not

- Spotify's own services and APIs.
- Bugs in the upstream [librespot](https://github.com/librespot-org/librespot)
  crates. Those go to that project. If a bug is in the patched copies under
  `vendor/`, report it here.

## Supported versions

Only the latest commit on `main` is supported.
