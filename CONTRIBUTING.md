# Contributing to memoryOSS

Thank you for your interest in contributing to memoryOSS!

## Contributor License Agreement (CLA)

Before we can accept your contribution, you must sign our [Contributor License Agreement](CLA.md).

When you open your first pull request, the CLA Assistant bot will ask you to sign. This is a one-time process — once signed, it covers all future contributions.

### Why a CLA?

memoryOSS is licensed under AGPL-3.0. We use dual licensing to fund development:

- **Community:** AGPL-3.0 — free forever, copyleft
- **Enterprise:** Commercial license — for companies that can't use AGPL

The CLA allows us to offer both licenses. Without it, we'd need every contributor's permission to offer the commercial license, which doesn't scale.

Your contributions remain credited to you. You retain copyright. The CLA grants us a license to use your contributions under any license, including proprietary ones.

## How to Contribute

1. Fork the repo
2. Create a feature branch (`git checkout -b feat/my-feature`)
3. Commit with sign-off (`git commit -s -m "feat: add something"`)
4. Push and open a PR
5. Sign the CLA when prompted by the bot

## Code Style

- Rust: `cargo fmt` + `cargo clippy` must pass
- Commits: [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, etc.)
- Tests: New features need tests. Bug fixes need a regression test.

## Reporting Issues

Use [GitHub Issues](https://github.com/memoryOSScom/memoryoss/issues). Include:

- What you expected
- What happened
- Steps to reproduce
- memoryOSS version (`memoryoss --version`)

## Security

See [SECURITY.md](SECURITY.md) for reporting vulnerabilities.
