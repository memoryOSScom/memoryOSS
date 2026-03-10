# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Reporting a Vulnerability

We take security seriously. If you discover a vulnerability in memoryOSS, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

### How to Report

1. Email: **security@memoryoss.com**
2. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

### What to Expect

- **Acknowledgment** within 48 hours
- **Initial assessment** within 5 business days
- **Fix timeline** communicated within 10 business days
- **Credit** in the security advisory (unless you prefer anonymity)

### Scope

In scope:
- memoryOSS server binary
- REST API endpoints
- MCP server
- Authentication/authorization (JWT, RBAC, API keys)
- Encryption at rest (AES-256-GCM)
- TLS implementation
- Python and TypeScript SDKs

Out of scope:
- Third-party dependencies (report upstream)
- Denial of service via resource exhaustion (rate limiting exists)
- Issues requiring physical access to the host

## Important Notes

- **Dev mode** (`memoryoss dev`) disables authentication and binds to localhost only. Never expose dev mode to a network.
- **`passthrough_auth`** forwards client API keys to upstream LLM providers. The wizard enables this for local use. Disable it if you don't need proxy functionality.
- **API keys** are stored in the config file. Protect the config file with appropriate file permissions (the wizard sets `0600`).

## Security Architecture

### Defense Layers

1. **Transport**: TLS 1.3 (rustls), optional mTLS
2. **Authentication**: API key → JWT exchange, scoped tokens
3. **Authorization**: RBAC (reader/writer/admin) per namespace
4. **Encryption**: AES-256-GCM at rest, per-namespace keys
5. **Audit**: Hash-chained tamper-proof audit log
6. **Trust**: Bayesian trust scoring, content hash dedup
7. **Input validation**: Size limits, rate limiting, sanitization

### OWASP API Security Top 10 — Self-Audit

| # | Risk | Status | Implementation |
|---|------|--------|----------------|
| API1 | Broken Object Level Authorization | Mitigated | Namespace isolation, JWT namespace claims |
| API2 | Broken Authentication | Mitigated | JWT with expiry, API key rotation, scoped tokens |
| API3 | Broken Object Property Level Authorization | Mitigated | RBAC enforced on all endpoints |
| API4 | Unrestricted Resource Consumption | Mitigated | Rate limiting (configurable per key), max content size |
| API5 | Broken Function Level Authorization | Mitigated | Role-based endpoint access (reader/writer/admin) |
| API6 | Unrestricted Access to Sensitive Business Flows | Mitigated | Audit log on all operations, trust scoring |
| API7 | Server Side Request Forgery | Mitigated | Proxy makes outbound requests to configured upstream only (not user-controlled URLs), path validation blocks traversal |
| API8 | Security Misconfiguration | Mitigated | Security headers on all responses, TLS by default |
| API9 | Improper Inventory Management | Mitigated | Single binary, /health endpoint, Prometheus metrics |
| API10 | Unsafe Consumption of APIs | Mitigated | MCP input sanitization, content size limits |

### Pentest Preparation

- [ ] Budget allocated: ~2-5k EUR for external pentest
- [ ] Scope: REST API, MCP server, auth flow, encryption
- [ ] Provider: TBD (post-alpha)
- [ ] Pre-test: Run OWASP ZAP automated scan
- [ ] Post-test: Fix all High/Critical within 7 days
