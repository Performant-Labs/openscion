# Security policy

## Supported versions

open-scion is an early proof of concept. Only the latest commit on `main` is
supported, and it is not meant for production use yet.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub:
[**Report a vulnerability**](https://github.com/bnsd55/open-scion/security/advisories/new)
(the *Security* tab of this repository).

Include what you found, how to reproduce it, and what an attacker could do with
it. You will get an answer within seven days. Once a fix is ready, the report
is published as a security advisory, with credit to you unless you prefer
otherwise.

## Scope

Known limits of the current design are documented in the code as `SHORTCUT:`
comments and in the README. For example: there is one shared token and no TLS,
and the server refuses non-loopback addresses unless `--insecure-listen` is
given. A report about a documented limit is still welcome if it shows an impact
the documentation does not mention.
