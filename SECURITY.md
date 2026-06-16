# Security Policy

## Reporting a Vulnerability

Kirra is safety-critical infrastructure. Security vulnerabilities
should be reported privately.

**Do not open a public GitHub issue for security vulnerabilities.**

Report vulnerabilities to: justin.looney@kirrasystems.com

Please include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if known)

We will respond within 48 hours and work with you on a coordinated
disclosure timeline.

## Security Invariants

Kirra maintains the following security invariants.
Any bypass of these is a critical vulnerability:

- `KIRRA_ADMIN_TOKEN` compared with `constant_time_compare` only
- `require_admin_token` returns 503 on absent/empty token (never 200)
- `verify_attestation` uses real HMAC-SHA256 (never mocked)
- DDS actuator topics use `Volatile` durability (never `TransientLocal`)
- `OperationalCommand::Unknown` denied in all posture states
