# clean-my-mac

A rule-based macOS disk cleaner for a developer machine: it finds what actually eats the disk
there (DB dumps, stale agent worktrees, old `node_modules`/`target`, AI-tool VM bundles,
package-manager caches, simulator runtimes, Docker) and explains every finding. No AI at runtime;
the same disk always gives the same report.

It comes as a desktop app (Tauri) and a CLI, `cmm`, built on the same Rust core.

## Install with Homebrew

```bash
brew tap zak-roz/tap
```

```bash
brew trust --cask zak-roz/tap/clean-my-mac
```

```bash
brew install --cask zak-roz/tap/clean-my-mac
```

Homebrew 7 and later load casks from third-party taps only once you trust them; `brew trust`
above trusts this one cask, not the whole tap. You get:

- `clean-my-mac.app` in `/Applications` (universal: Apple silicon and Intel, macOS 13 or later);
- `cmm` on your `PATH`: a link to the CLI inside the app, so the two always have the same version.

**First launch.** Releases are signed with the project's own certificate but are **not
notarized** by Apple, so macOS blocks the first launch. Open **System Settings → Privacy &
Security** and click **Open Anyway** next to the message about clean-my-mac. Do this before you
use `cmm`: until then macOS stops it at once, without output (the shell prints `killed`, exit
status 137). If `cmm` is still stopped after that, allow it in the same place.

**Full Disk Access.** Grant it to the app as described [below](#full-disk-access). Every release is
signed with the same certificate, so updates keep the grant.

Update with `brew upgrade --cask clean-my-mac`. To uninstall:

```bash
brew uninstall --cask clean-my-mac
```

This quits and removes the app, `cmm` and the daily background scan (if you turned it on), and
keeps your history and settings. To delete those too (`~/Library/Application Support/clean-my-mac`
and the app's caches and preferences):

```bash
brew uninstall --zap --cask clean-my-mac
```

Without Homebrew: download `clean-my-mac.zip` from the
[latest release](https://github.com/Zak-Roz/clean-my-mac/releases/latest), unzip it and move the
app to `/Applications`. `cmm` is in `clean-my-mac.app/Contents/MacOS/`.

## Build from source

Needs Rust 1.97, Node 24, and Xcode command line tools (`corepack` comes with Node).

```bash
corepack pnpm install
```

```bash
scripts/install.sh --build
```

This builds the app with the CLI inside it (`scripts/build-app.sh`), then installs:

- `clean-my-mac.app` → `/Applications` (ad-hoc signed and not notarized, so it's meant for the
  Mac that built it);
- a `cmm` link → `~/.local/bin` pointing at the CLI inside the app (the folder is created if
  missing; you get a warning with the fix if it is not on your `PATH`).

Re-running it upgrades both. It refuses to replace anything that isn't clean-my-mac's own, and
refuses to replace the app while it's running. `--prefix DIR` installs into `DIR/Applications`
and `DIR/bin` instead, for a trial run. Without `--build` it installs the app already in
`target/release/bundle/macos`.

To uninstall:

```bash
scripts/uninstall.sh
```

This removes the app, the `cmm` link and, if it runs that app's `cmm`, the daily background scan;
nothing else. Your history and settings in `~/Library/Application Support/clean-my-mac` are kept
unless you pass `--purge`, which lists what it will permanently delete and asks you to type
`purge` to confirm.

## Full Disk Access

Grant it once: **System Settings → Privacy & Security → Full Disk Access → + →
/Applications/clean-my-mac.app**. The app's Overview has a button that opens that pane.

Why it's needed:

- **Trash size.** `~/.Trash` is privacy-protected. Without access, the app can't show how much the
  Trash holds. That number matters because moving things to the Trash frees nothing until it's
  emptied.
- **Protected caches and containers.** Some folders under `~/Library` (e.g.
  `~/Library/Caches/familycircled`, Safari, Mail, other apps' containers) can't be read. Their
  sizes are reported as errors, and the safety guard **refuses to clean any folder it can't fully
  read**. `~/Library/Caches` as a whole is skipped, for example, while individual caches inside
  still work.

Everything else works without it.

Two things to know:

- Homebrew releases keep the permission across updates. A build from source is ad-hoc signed, so
  macOS ties the permission to that exact build: **after every reinstall from source, remove the
  old clean-my-mac entry (−) and add the app again**; otherwise the toggle shows as on but doesn't
  apply. The same goes for switching between a release and a build from source.
- The CLI inherits the permission of the terminal it runs in. For `cmm`, give Full Disk Access
  to Terminal, iTerm, or whichever terminal you use.

## Usage

**App.** Overview (free space, swap, Trash, growth since earlier scans) → **Scan** → Findings
(grouped by category, with risk, size and the reason for each) → tick items → Confirm (Trash or
Delete) → History. **Storage** ("What's using space?" on the Overview) explains the rest of the
used space, which no scan rule targets: macOS volumes, apps, `~/Library`, your folders and what
could not be measured, with drill-down into folders. It only measures; it never deletes.

**CLI.**

```bash
cmm scan                         # table of findings; --json, --category <c>, --rule <id>
cmm rules                        # the rule catalog
cmm clean --safe                 # dry run: what the safe, preselected items would do
cmm clean --id <finding-id> --yes            # move to the Trash
cmm clean --rule cache.npm --delete --yes    # delete permanently
cmm history --days 30            # growth by rule/item and the free-space trend
cmm du ~/Developer --top 20      # quick allocated-size breakdown
cmm storage --depth 1            # what the whole used space is made of (walks the disk)
```

Data lives in `~/Library/Application Support/clean-my-mac`: `history.sqlite` (scans and the
audit log), `last-report.json`, `last-storage.json` (the Storage view's last measurement), an
optional `config.toml`, and `rules.d/*.toml` to add, override or disable rules by id.

## Safety model

- **Trash by default.** Permanent deletion only when you choose Delete (`--delete`). Moving to the
  Trash frees no space until the Trash is emptied, and results report that separately.
- **Dry run by default.** The CLI changes nothing without `--yes`. The app shows a plan first.
- **Fresh re-scan before acting.** A clean never trusts the report you looked at. It re-scans
  the affected rules and acts only on items that are unchanged: same id, action and command, no
  blockers, size within max(10 %, 10 MiB), and still a file or still a folder. Anything else is
  skipped with the reason.
- **Path guard.** Targets must be inside your home folder. They may never be (or contain) home
  itself, Documents, Desktop, Downloads, Developer, Library, Application Support, Keychains,
  iCloud Drive, `.ssh`, `.gnupg` or the app's data. They may never contain a `.git` entry
  (only a worktree's own `.git` file is allowed). Symlinks are never followed.
- **Blockers.** Items whose app is running (Claude, Docker, …), dirty or unpushed git worktrees,
  and similar are shown but can't be cleaned.
- **Risk levels.** *safe* items (caches) are preselected. *review* items need a tick. *manual*
  items (dumps, media, documents) are never preselected or bulk-selected.
- **Allowlisted commands only.** `docker system prune -f`, `xcrun simctl …` and
  `git worktree remove` must match an exact argv pattern. There is no "empty Trash".
- **Audit log.** Every executed action is recorded in the history database. Cleaning refuses to
  run if that database can't be opened.

Details: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §6 and §10.

## Releasing

GitHub Actions builds, signs and publishes a release when a `v*` tag is pushed
([`release.yml`](.github/workflows/release.yml)); [`ci.yml`](.github/workflows/ci.yml) runs the
checks on every push to `main` and every pull request.

Once, on the repository owner's Mac (needs `gh` with admin rights on this repository and on
`<owner>/homebrew-tap`; `--dry-run` shows the plan and changes nothing):

```bash
scripts/setup-release-signing.sh --dry-run
```

```bash
scripts/setup-release-signing.sh
```

It creates the self-signed code signing certificate "clean-my-mac Release", keeps a backup in
`~/Library/Application Support/clean-my-mac-release-signing` (copy it somewhere safe: a release
signed with another certificate makes every user grant Full Disk Access again), and sets the
repository secrets `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD` and `HOMEBREW_TAP_DEPLOY_KEY` (a write
deploy key on the tap).

Then, for each release:

```bash
scripts/release.sh 0.2.0
```

```bash
git push --atomic origin main v0.2.0
```

`release.sh` sets the version in `Cargo.toml`, `Cargo.lock`, `tauri.conf.json` and `package.json`,
commits `release: v0.2.0` and tags `v0.2.0`; it pushes nothing. The workflow checks the tag
against those files, builds a universal app with `cmm` inside, signs it with the certificate,
publishes `clean-my-mac.zip` on a GitHub release with generated notes, and updates
`Casks/clean-my-mac.rb` in the tap (rendered from
[`packaging/homebrew/clean-my-mac.rb.tmpl`](packaging/homebrew/clean-my-mac.rb.tmpl)). A tag like
`v0.2.0-beta.1` makes a pre-release, which Homebrew skips. `scripts/build-app.sh --universal`
builds the same app locally, signed ad hoc.

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p cmm-cli -- scan                 # read-only
corepack pnpm install
corepack pnpm -w lint && corepack pnpm -w typecheck
corepack pnpm --filter desktop test          # Vitest
corepack pnpm --filter desktop tauri dev     # the app; see apps/desktop/README.md for mock mode
```

The TypeScript types in `apps/desktop/src/bindings/` are generated from the Rust model by ts-rs
(`cargo test -p cmm-core export_bindings`); don't edit them by hand. Tests build their fixtures in
temporary directories and never touch your home folder.

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describes the design: data model, core API, rule
format, detectors, safety invariants and the decisions behind them.
