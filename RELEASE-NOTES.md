# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.6.0 — 2026-11-04

Help inside the console: every one of the twelve system pages now ends with a **Getting started** tab and a **FAQ**
tab.

- **Getting started:** what the system is and what the console does with it, then numbered steps from install to first
  use. Steps carry the exact command where one helps (with a Copy button; the console never runs it) and a button that
  opens the tab the step talks about.
- **FAQ:** the system's common questions, mostly the problem codes and refusals the status and actions can show, and
  below them the questions that apply on every page (greyed-out buttons, off / down / unknown, the audit trail, empty
  charts after a restart, the one-time reload after an upgrade). A filter narrows the list and opens what matches.
- **How it is built:** the content is one plain-text file, `frontend/i18n/help.en.json`, loaded the first time a help
  tab opens and rendered as text only. The tabs are added once in the shared page frame, so a future system page gets
  them too. A link to a tab that a page did not build (Config and Playground are optional on some pages) is not shown.
- **How it was checked:** a new frontend test checks that every system has a guide and an FAQ and that every tab and
  page link is real; the browser suite now opens 77 pages and tabs (53 before); the filter, Expand all and the tab
  links were driven in headless Chrome. The text was written from the operations guide and the feature matrix, not
  re-checked against every live system.
- **Unchanged:** the backend's behaviour. Only its version number moved.

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
A browser tab that was open across the upgrade picks up the new files on its next load.
