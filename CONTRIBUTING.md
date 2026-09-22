# Contributing

Thanks for looking. This is an offline English-practice app: conversation
practice and interview practice, running entirely on the user's machine.

## Scope

English only, on purpose. The speech recognition model, the voice, and the
grammar rules are all English-specific, and "add another language" means
finding, licensing, hashing and shipping a new model for each subsystem.
Multi-language support is out of scope; a fork is a reasonable answer.

Anything that sends user audio, transcripts or review history off the machine
is out of scope too. The only network request the app makes is downloading
model files from a pinned URL.

## Getting set up

Requires Rust (stable, MSRV 1.77), Node 22.22.2 or newer, and `cmake` (the
bundled espeak build needs it). On Linux also install the Tauri system
dependencies listed in `.github/workflows/ci.yml`.

```bash
npm install
./scripts/download-models.sh   # ~456 MB; or let the app fetch them on first run
npm run tauri dev
```

One terminal is enough — `beforeDevCommand` starts Vite.

On Linux with an NVIDIA GPU under Wayland, WebKitGTK's DMA-BUF renderer can
crash the window at launch. If that happens:

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev
```

## Before you push

```bash
just check      # or the four commands below
```

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked --all-targets
npm test
```

CI runs exactly these, plus a cross-platform compile on Linux, macOS and
Windows, and a check that `package.json`, `tauri.conf.json` and `Cargo.toml`
all report the same version.

## House rules

These exist because breaking them has already cost real debugging time.

**Clippy warnings are errors.** The crate is built as `staticlib`/`cdylib`
too, so an unreferenced `pub` item is a hard error, not a warning. A helper
written "for the next commit" will fail the build.

**MSRV is 1.77.** `#[allow(…, reason = "…")]` needs 1.81 and must not be used.

**No `ALTER TABLE` after migration 4.** SQLite has no
`ADD COLUMN IF NOT EXISTS`, and a partially-failed migration re-runs. Extend
the schema with sidecar tables and `LEFT JOIN`. Every statement must be
`CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS` or
`INSERT OR IGNORE`. A new `MIGRATION_N_SQL` in `db.rs` must also be
registered in the `Migration` vec in `lib.rs::run`.

**Deletes remove children explicitly.** `PRAGMA foreign_keys` is per
connection and the SQL plugin owns the pool, so cascade cannot be relied on.
Delete child rows first, in one transaction.

**Never fabricate a score.** Pronunciation is only scored where a reference
text exists. Open-ended answers show "Not scored for free speaking" rather
than a number that means nothing.

**Adding a backend command means touching four files.** `lib.rs` (the command
plus its entry in `generate_handler!`), then `src/ipc/types.ts`,
`src/ipc/commands.ts` and `src/ipc/mock.ts`. `contract.test.ts` fails
otherwise, by design. Nothing outside `src/ipc/` may call `invoke` or
`listen`.

**Every phase stays green without model files.** New logic belongs in pure
modules or in-memory SQLite tests. Tests that need the network are
`#[ignore]`d and named `network_*`; run them with `just test-net`.

## Models

Model URLs are pinned to immutable commits and verified by sha256. The same
values live in `src-tauri/src/download.rs` and `scripts/download-models.sh`;
if you re-pin a revision, update both and run `just test-net`, which is the
only check that the pinned URLs still resolve to the bytes the catalog
expects. See SECURITY.md for why this is strict.

## Commits and pull requests

Conventional-style subjects (`feat(srs):`, `fix(ui):`, `ci:`) in the
imperative mood. Say what changed and why; the diff already says how.

Small PRs get reviewed faster. If you are planning something large, open an
issue first so the design can be argued about before you write it.
