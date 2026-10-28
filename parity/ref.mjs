#!/usr/bin/env node
// The Python reference for the parity harness. Since 0.4.0 the checkout holds only the Rust backend; the last Python
// backend stays the behavioural reference. `python@<rev>` (tag python-final, a tag such as v0.3.1, or a commit id;
// default `python` = python@python-final) extracts that revision's backend/ with `git archive` into parity/.ref/<rev>/ (gitignored, kept
// between runs, never a git worktree) and creates its venv there ONCE with `uv sync --frozen` (Python 3.13, the
// revision's own uv.lock).
//
//   node parity/ref.mjs [tag]     prepare the reference and print its backend dir (for the golden generators:
//                                 REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I parity/gen/<gen>.py ...)
//
// Network: `uv sync` runs OFFLINE (UV_OFFLINE=1, from the uv cache) unless PARITY_UV_ONLINE=1 allows PyPI for a first
// install on a new machine. Nothing else is fetched; a tag other than python-final must exist locally (`git fetch --tags`).
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const PARITY = dirname(fileURLToPath(import.meta.url));
export const WORKTREE = resolve(PARITY, '..');
/** The last Python backend: tag `python-final` = main at aa2f810 (version 0.3.1 + two unreleased commits: deployment
 *  names as settings, the opt-in capable-tier probe), the code the Rust port was checked against. Tag v0.3.1 is two
 *  commits older, so parity against it differs on those settings. */
export const DEFAULT_REF = 'python-final';
/** The commit `python-final` must name. A checkout without the tag (`git fetch --tags` not run) uses this commit; a tag
 *  that names any other commit is refused, so a moved tag cannot silently change the reference. */
export const PYTHON_FINAL_COMMIT = 'aa2f810a951fa11a42381e98df6b3ec579a813e6';
const REF_ROOT = join(PARITY, '.ref');
const TAG_RE = /^(?:python-final|v\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?|[0-9a-f]{7,40})$/;

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, ...opts });
  if (r.error) throw new Error(`${cmd} ${args.join(' ')}: ${r.error.message}`);
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} exited ${r.status}:\n${(r.stderr || '').slice(-2000)}`);
  return r.stdout;
}

/** `python` | `python@<tag>` -> the tag. */
export function refTag(spec) {
  if (spec === 'python') return DEFAULT_REF;
  const m = /^python@(.+)$/.exec(spec || '');
  if (!m) throw new Error(`not a Python reference: ${spec} (python | python@<tag or commit>)`);
  if (!TAG_RE.test(m[1])) throw new Error(`reference must be python-final, a tag vX.Y.Z or a commit id, not ${m[1]}`);
  return m[1];
}

/**
 * The Python reference of `tag`, prepared on first use: { tag, backendDir, python, version }.
 * A half-built reference (an interrupted extract or venv) is rebuilt from scratch.
 */
/** `python-final` -> the commit it must name (checked when the tag exists locally); any other ref unchanged. */
export function resolveRef(tag) {
  if (tag !== DEFAULT_REF) return tag;
  const r = spawnSync('git', ['-C', WORKTREE, 'rev-parse', '--verify', '--quiet', `refs/tags/${tag}^{commit}`], { encoding: 'utf8' });
  const named = r.status === 0 ? r.stdout.trim() : '';
  if (named && named !== PYTHON_FINAL_COMMIT) {
    throw new Error(`tag ${tag} names ${named}, not ${PYTHON_FINAL_COMMIT}: refusing a moved reference`);
  }
  return PYTHON_FINAL_COMMIT;
}

export function pythonRef(tag = DEFAULT_REF) {
  if (!TAG_RE.test(tag)) throw new Error(`reference must be python-final, a tag vX.Y.Z or a commit id, not ${tag}`);
  tag = resolveRef(tag);
  const dir = join(REF_ROOT, tag);
  const backendDir = join(dir, 'backend');
  const python = join(backendDir, '.venv', 'bin', 'python');
  const done = join(dir, '.ready');
  if (!existsSync(done)) {
    // built in place (a venv records absolute paths, so it cannot be moved after creation); `.ready` marks success
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    run('git', ['-C', WORKTREE, 'rev-parse', '--verify', '--quiet', `${tag}^{commit}`]);
    // the tag's backend/ only (no frontend: both sides serve this checkout's frontend/)
    const tar = spawnSync('git', ['-C', WORKTREE, 'archive', '--format=tar', tag, 'backend'], { maxBuffer: 512 * 1024 * 1024 });
    if (tar.status !== 0) throw new Error(`git archive ${tag} backend failed: ${String(tar.stderr).slice(-500)}`);
    const x = spawnSync('tar', ['-x', '-C', dir], { input: tar.stdout });
    if (x.status !== 0) throw new Error(`tar -x failed: ${String(x.stderr).slice(-500)}`);
    const env = { ...process.env, UV_PROJECT_ENVIRONMENT: join(backendDir, '.venv') };
    if (process.env.PARITY_UV_ONLINE !== '1') env.UV_OFFLINE = '1';
    run('uv', ['venv', '--quiet', '--python', '3.13', join(backendDir, '.venv')], { cwd: backendDir, env });
    run('uv', ['sync', '--quiet', '--frozen'], { cwd: backendDir, env });
    writeFileSync(done, `${tag}\n`);
  }
  const m = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(backendDir, 'pyproject.toml'), 'utf8'));
  return { tag, backendDir, python, version: m ? m[1] : tag.slice(1) };
}

/** The Rust backend's version (workspace Cargo.toml). */
export function rustVersion() {
  const m = /^\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/ms.exec(readFileSync(join(WORKTREE, 'backend', 'Cargo.toml'), 'utf8'));
  return m ? m[1] : '';
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    console.log(pythonRef(process.argv[2] || DEFAULT_REF).backendDir);
  } catch (e) {
    console.error(`ref: ${e.message}`);
    process.exit(1);
  }
}
