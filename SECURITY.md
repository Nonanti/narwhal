# Security Policy

## Supported Versions

narwhal follows Semantic Versioning. Security fixes land on the latest
minor of the most recent major release.

| Version | Supported |
|---------|-----------|
| 1.x     | Yes       |
| < 1.0   | No        |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security problems.**

Email: **nonantiy1@gmail.com**

Include in the report:

- Affected version (`narwhal --version`)
- Affected component (driver, MCP server, plugin runtime, config loader, …)
- Reproduction steps or a minimal proof-of-concept
- Impact assessment (read leak, write bypass, RCE, DoS, …)
- Whether you'd like public credit in the changelog

### What to expect

- **Acknowledgement** within 72 hours.
- **Triage + severity assessment** within 7 days.
- **Fix + coordinated disclosure** target: 30 days for high/critical, 90 days for medium/low.
- A CVE will be requested through GitHub Security Advisories for any
  issue scored medium or higher (CVSS 4.0+).

## Scope

In scope:

- MCP server read-only guard bypass (`narwhal mcp`)
- SQL injection through narwhal's own quoting/formatting code
- Credential leakage (history journal, logs, error messages, status bar)
- Plugin sandbox escape (Lua runtime, `narwhal.sql_run` bridge)
- TLS validation gaps in any bundled driver
- Workspace ACL bypass (`.narwhal/workspace.toml`)
- Privilege escalation through `pre_connect` command execution

Out of scope:

- Vulnerabilities in upstream dependencies — please report those to
  the upstream project. We'll bump the version in a follow-up PR.
- Misconfiguration where the user explicitly disabled a safety
  (`--write`, `allow_writes = true`, `set_read_only(false)`).
- Local attacker with read access to `~/.config/narwhal/` —
  narwhal's threat model assumes the home directory is trusted.

## Hardening notes for operators

- Run `narwhal mcp` with `--read-only` unless you specifically need writes.
- Scope MCP exposure with `.narwhal/workspace.toml` (`allowed_connections`).
- Set `ssl_mode = "verify-full"` for any non-localhost connection.
- Store credentials in the OS keyring, not as plaintext in
  `connections.toml`. The wizard does this by default.
- Pin plugin scripts in `~/.config/narwhal/plugins/` to 0600 perms.
