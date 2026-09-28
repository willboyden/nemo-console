# ADR-0013: Zero-build, zero-npm frontend with a strict CSP

Status: Accepted (v0.1.0)

## Context

A control plane that can restart containers should not pull hundreds of transitive npm packages into its browser
bundle. It also serves a page that holds an operator bearer key in browser storage.

## Decision

Plain ES modules and web components, no bundler, no framework, no CDN, hand-written SVG charts, one locale
(`i18n/en.json`). Files are served from a static **allowlist** (`index.html`, `favicon.*`, and `css/ js/ assets/
img/ fonts/ i18n/` with a subpath); `dev/` and `tests/` are never served, `../` and symlink escapes are refused.
Every response carries `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self';
connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`.
The DOM helper `h()` refuses a `style` attribute and builds text via `textContent`; `innerHTML` is only used by
`setTrustedHtml`, whose callers pass chart-builder output that escapes dynamic text. An optional bearer is kept in
`sessionStorage` only; the theme in `localStorage`.

## Consequences

- No supply-chain surface from npm at runtime; `package.json` exists only so Node treats `js/` as ES modules
  for `node --test`.
- `frontend/tests/static-guards.test.mjs` fails the build on inline handlers, style attributes, `eval`,
  external origins, `innerHTML` outside `dom.js`, missing i18n keys, and unresolved imports.
- Browser coverage: frontend rendered against the real backend in headless Firefox (verified live). Other
  browsers and assistive technology are unverified; ARIA tabs, live regions, a skip link, focus outlines and
  reduced-motion CSS exist but no screen reader was used.
- The favicon is a `data:` URI (allowed by `img-src`).
- Hand-rolled charts and no framework mean more code to maintain and no ecosystem components.
- English only.

## Alternatives considered

- **React/Vite.** Rejected: build step and npm tree.
- **Server-rendered HTML (htmx-style).** Would need a script dependency or hand-written swaps; SPA over JSON
  keeps the API the single contract.
- **Chart library (uPlot, Chart.js).** Rejected for supply-chain reasons; the charts needed are simple.
