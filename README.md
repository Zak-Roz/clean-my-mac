# clean-my-mac

A rule-based macOS disk cleaner for a developer machine: it finds what actually eats the disk
there (DB dumps, stale agent worktrees, old `node_modules`/`target`, AI-tool VM bundles,
package-manager caches, simulator runtimes, Docker) and explains every finding. No AI at runtime;
the same disk always gives the same report.

It comes as a desktop app (Tauri) and a CLI, `cmm`, built on the same Rust core.

## Install

Needs Rust 1.97, Node 24, and Xcode command line tools (`corepack` comes with Node).

```bash
corepack pnpm install
```

```bash
scripts/install.sh --build
```

This builds the release CLI and app, then installs:

- `clean-my-mac.app` → `/Applications` (ad-hoc signed and not notarized, so it's meant for the
  Mac that built it);
- `cmm` → `~/.local/bin` (created if missing; you get a warning with the fix if it is not on your `PATH`).

Re-running it upgrades both. It refuses to replace anything that isn't clean-my-mac's own, and
refuses to replace the app while it's running. `--prefix DIR` installs into `DIR/Applications`
and `DIR/bin` instead, for a trial run. Without `--build` it installs what's already in
`target/release`.

To uninstall:

```bash
scripts/uninstall.sh
```

This removes the app and the CLI and nothing else. Your history and settings in
`~/Library/Application Support/clean-my-mac` are kept unless you pass `--purge`, which lists
what it will permanently delete and asks you to type `purge` to confirm.

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

- The build is ad-hoc signed, so macOS ties the permission to this exact build. **After every
  reinstall, remove the old clean-my-mac entry (−) and add the app again**; otherwise the toggle
  shows as on but doesn't apply.
- The CLI inherits the permission of the terminal it runs in. For `cmm`, give Full Disk Access
  to Terminal, iTerm, or whichever terminal you use.

## Usage

**App.** Overview (free space, swap, Trash, growth since earlier scans) → **Scan** → Findings
(grouped by category, with risk, size and the reason for each) → tick items → Confirm (Trash or
Delete) → History.

**CLI.**

```bash
cmm scan                         # table of findings; --json, --category <c>, --rule <id>
cmm rules                        # the rule catalog
cmm clean --safe                 # dry run: what the safe, preselected items would do
cmm clean --id <finding-id> --yes            # move to the Trash
cmm clean --rule cache.npm --delete --yes    # delete permanently
cmm history --days 30            # growth by rule/item and the free-space trend
cmm du ~/Developer --top 20      # quick allocated-size breakdown
```

Data lives in `~/Library/Application Support/clean-my-mac`: `history.sqlite` (scans and the
audit log), `last-report.json`, an optional `config.toml`, and `rules.d/*.toml` to add, override
or disable rules by id.

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

## Development

See [`CLAUDE.md`](CLAUDE.md) for commands (`cargo test --workspace`, `corepack pnpm -w lint`,
`corepack pnpm --filter desktop tauri dev`, …) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
for the contract.
