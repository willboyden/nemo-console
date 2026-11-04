# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.7.0 — 2026-11-04

Help on every page: Overview, Audit and Settings now have the **Getting started** and **FAQ** tabs that the twelve
system pages got in 0.6.0.

- **Tabs on the console's own pages:** each page's content is its first tab (Overview: Status, Audit: Log,
  Settings: Settings), then Getting started and FAQ. Deep links such as `#/settings?tab=faq` work, and `/` still
  opens Overview on its first tab. Overview now polls only while its first tab is open.
- **What the help covers:** Overview (signing in, reading the topology, the system cards), Audit (what is and is
  not recorded, redaction, reading more rows through the API, backing up the log) and Settings (the tab key, URLs
  that need a restart, credentials by environment-variable name, the trust line). Each FAQ also lists the
  console-wide questions, minus the one that links back to the page you are on.
- **How it was checked:** the help test now covers the three pages; the browser suite opens 86 pages and tabs (77
  before); deep links, the `/` route, Overview's polling and Settings' key save were driven in headless Chrome.
- **Unchanged:** the backend's behaviour (only its version number moved).

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
