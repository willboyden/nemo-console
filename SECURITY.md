# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | Yes |

This is a single-maintainer project. Fixes land on `main` and in the next patch release.

## Reporting a vulnerability

Please report privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability**.
Do not open a public issue for anything exploitable.

Include the version, how the console is deployed (bind address, `trust_loopback`, reverse proxy if any), and the
steps or request that show the problem. Redact keys, tokens and hostnames from logs.

I aim to acknowledge a report within 7 days and to agree on a disclosure date with you. There is no bug bounty.

## Scope

In scope: the Nemo Console backend and frontend in this repository, including its authentication, CSRF and Host
checks, the command runner, config writes and the playground proxy.

Out of scope: NeMo Switchyard, NeMo Relay, NVIDIA Dynamo and the models they serve. Report those to their own
maintainers.

## Design notes

The console is meant for one operator on one workstation, bound to loopback. It can start, stop and reconfigure
containers, so treat an admin key like the operator's Docker access. The assets, trust boundaries and residual risks
are in [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md), and deployment hardening is in
[`docs/OPERATIONS.md`](docs/OPERATIONS.md).
