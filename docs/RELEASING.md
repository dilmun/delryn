# Releasing delryn

delryn uses an automated pipeline where the maintainer makes **exactly one decision**:

> **"Release now?" → merge the standing release PR.**

Everything else — the version bump, `CHANGELOG.md`, the `vX.Y.Z` tag, the GitHub
Release, and the per-platform binaries — is automatic. You never choose a version
number, write release notes, or push a tag by hand.

The engine is [**release-please**](https://github.com/googleapis/release-please), configured as
a **single "simple" release for the whole repo** (one package at path `.`). It is purely
Conventional-Commit-based — it never runs cargo and never touches a registry or `Cargo.lock` —
which fits delryn: a GitHub-only binary, **never published to crates.io**, built from ~10
internal path-dependency crates.

> **Why this shape (not release-plz, not per-crate release-please)?** delryn is one app in a
> virtual workspace, so it's released as **one unit**, not ten. Tools that version each crate
> from that crate's own history don't fit:
> - **release-plz** diffs each crate against its **crates.io-published** copy; delryn publishes
>   nothing, and its `git_only` fallback then runs `cargo package`, which can't diff an
>   unpublished path-dependency workspace — a
>   [tracked, still-open release-plz bug](https://github.com/release-plz/release-plz/issues/2595).
> - **per-crate release-please** (`rust` type + `cargo-workspace`/`linked-versions`) emits one
>   tag + one GitHub Release **per crate** (10 of each); collapsing them to one is itself an
>   [open, unmerged release-please bug](https://github.com/googleapis/release-please/pull/1749).
>
> `simple`-at-`.` sidesteps all of it: **any** releasable commit in **any** crate bumps **one**
> version and cuts **one** tag `vX.Y.Z`. See ["How the wiring works"](#how-the-wiring-works).

---

## How it works (the mental model)

There are two kinds of pull request, both merged the way you already merge any PR:

- **Your PRs** — features and fixes. You open them; they merge to `main`.
- **The release PR** — a *single* standing PR that the bot (`dilmun-release-bot`) opens
  and keeps up to date. Merging it **is** the release.

Nothing releases on its own. Landing features/fixes only *updates* the release PR; a
release happens **only** when you merge that PR.

### The loop

```
   you ship normal PRs to main ─────────────────────────────────────┐
                                                                     │
      merge  feat: themes   ─┐                                       │
      merge  fix:  crash    ─┤  each merge only UPDATES the release  │
      merge  feat: export   ─┤  PR — NO release happens              │
      merge  fix:  typo     ─┘                                       │
                             │                                       │
                             ▼   release-please keeps ONE PR current │
                ┌──────────────────────────────────────────┐        │
                │  📄  Release PR  →  v0.2.0                 │ grows  │
                │      • themes • export • crash • typo      │ over   │
                │      (version bump + CHANGELOG)            │ time   │
                └──────────────────────┬───────────────────┘         │
                                       │                             │
                     sits open ⏸ … until you decide …               │
                                       │                             │
                     👆 you click [Merge]  ← the one button          │
                                       ▼                             │
                ┌──────────────────────────────────────────┐        │
                │  🚀  RELEASE v0.2.0                        │        │
                │      tag + GitHub Release + binaries       │        │
                └──────────────────────┬───────────────────┘         │
                                       │                             │
                  next feat/fix starts a fresh cycle  ◄──────────────┘
```

### Where the release PR actually lives

It's a normal PR in the **Pull requests tab**, built on a bot-owned branch — never on
`main` itself:

```
   main:  A───B───C            ← your real code
                   ╲
                    R          ← branch  release-please--branches--main
                                 ONE commit: bump the version + write CHANGELOG
                    │
                    └──► PR:  [release-please--branches--main] ──merge into──► [main]
```

You never touch that branch. You only ever click **Merge** on the PR.

### How the bot branch gets created (you didn't — a workflow did)

```
   you merge a PR to main
          │
          ▼
   GitHub runs  .github/workflows/release-please.yml   (on: push to main)
          │
          ▼
   googleapis/release-please-action   🔑 using the GitHub App token
          │
          ▼
   the App (Contents + Pull requests + Issues) does the git for you:
      • git branch  release-please--branches--main
      • git commit  (version bump + CHANGELOG)
      • open / update the PR (+ its autorelease:* labels)
```

The workflow is the trigger; the App is the identity that runs `git`. That's why the
release PR is authored by `dilmun-release-bot`, not you.

---

## Day-to-day: landing a change

`main` is protected (required checks + `enforce_admins`), so you **cannot push to it
directly** and you **never merge into it locally** — a local merge would be rejected and
would skip the PR-title → squash → release-please chain. Every change goes through a branch
and a PR:

```
1. git checkout -b fix/pdf-crash        # branch = your private workspace
   …work, commit freely…                # branch commit messages don't matter (squashed away)
2. git push -u origin fix/pdf-crash      # push the BRANCH, not main
3. gh pr create --title "fix: …"         # open PR — the TITLE is what counts
   …CI + "Validate PR title" run…        # go green
4. gh pr merge --squash --delete-branch  # the merge IS the "push to main"
5. git checkout main && git pull         # sync local main
```

| Step | Command | Why |
| ---- | ------- | --- |
| Branch | `git checkout -b <type>/<slug>` | Never work on `main`. Name by intent: `feat/…`, `fix/…`, `docs/…`. |
| Push | `git push -u origin <branch>` | Publishes the branch; `-u` links it to remote. A direct push to `main` is rejected. |
| PR | `gh pr create --title "feat: …"` | The **PR title becomes the squash commit** — the one Conventional Commit release-please reads. |
| Merge | `gh pr merge --squash --delete-branch` | Squash → one clean commit on `main`; auto-deletes the branch. Add `--auto` to merge the moment checks pass. |
| Sync | `git checkout main && git pull` | Pull the merged commit back down. |

**The one rule that saves you grief:** only the **PR title** must be a valid Conventional
Commit (`feat:`, `fix:`, `docs:`, …). Your branch commits are squashed and discarded, so
commit `wip` / `typo` all you like — just title the *PR* correctly.

### Before you push (fast local check)

CI runs these on every PR regardless, but running them locally first catches problems in
**seconds** instead of waiting on a red CI round-trip:

```sh
cargo fmt --all --check                                 # formatting
cargo clippy --workspace --all-targets -- -D warnings   # lints — warnings fail the build
cargo test --workspace                                  # tests
```

`main` must stay green — build + `cargo test` + `clippy` (0 warnings) + `cargo fmt` — on
every merge; CI + branch protection guarantee it, so a red check simply can't land. Skip
these only for docs-/CI-only changes with no code to break.

---

## Commit & PR-title convention

Because we squash-merge, the **PR title is the commit standard** — it's the single commit
that lands on `main`, the Conventional Commit release-please parses, and the changelog line
users read. The required `Validate PR title` check enforces it on every PR (branch commits
are informal — they're squashed away).

### Format

```
type(scope): short description
```

- **type** — required; one of the set below.
- **(scope)** — optional; the delryn area touched (see list).
- **description** — imperative mood, lowercase, no trailing period, ≤ ~70 chars
  (`add …`, not `Added …` / `adds …`).
- **breaking change** — add `!` after the type/scope (`feat(store)!: …`), or a
  `BREAKING CHANGE:` line in the PR body.

### Types

| type | use for | release? |
| --- | --- | --- |
| `feat` | a new user-facing capability | **minor** |
| `fix` | a bug fix | **patch** |
| `perf` | performance improvement | none\* |
| `refactor` | internal restructure, no behavior change | none |
| `docs` | documentation only | none |
| `test` | tests only | none |
| `build` | build system / dependencies | none |
| `ci` | CI / workflows | none |
| `chore` | maintenance, no source-behavior change | none |
| `style` | formatting / whitespace only | none |
| `revert` | revert a previous change | none |

\* release-please never bumps on `perf`/`refactor`, and "user-visible" isn't
machine-detectable for a binary. **If users would notice the change, label it `fix` or
`feat`** — that's the only way it reaches a release and the changelog. See
[Release version convention](#release-version-convention-semver) for the exact bump rules.

### Scopes (delryn areas — optional, encouraged)

Use the area a *user* would recognize, not the crate name; omit if it spans many:

`reader` · `library` · `pdf` · `epub` · `mobi` · `math` · `render` · `layout` · `images` ·
`annotations` · `search` · `settings` · `status` · `theme` · `store` · `config` · `mouse` ·
`overlay`

### Examples

```
✓ feat(reader): remember last position per book
✓ fix(pdf): stop crash when opening encrypted files
✓ feat(annotations)!: change highlight storage format     ← breaking (minor while 0.x)
✓ docs: expand release guide
✓ perf(render): cache wrapped lines                       ← no release; if users feel it, use fix/feat

✗ update stuff                 → no type; fails the check
✗ Fixed the bug.               → capitalized, past tense, trailing period; use "fix: …"
✗ feat: Added a big new thing that lets the reader …      → capitalized + too long
```

### Why no commit-lint hooks

Enforcement is the one server-side `Validate PR title` check — deliberately no local
`commit-msg` hooks or `commitlint`. With squash-merge only the PR title matters, and a
single required check beats per-machine hook setup that every contributor must install.

---

## Release version convention (SemVer)

delryn follows [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`. release-please
computes the next version automatically from the Conventional Commit **types** merged since
the last release tag — you never pick a number.

| PR-title type | Bump | Example |
| --- | --- | --- |
| `fix:` | **patch** | 0.3.1 → 0.3.2 |
| `feat:` | **minor** | 0.3.1 → 0.4.0 |
| `feat!:` / `BREAKING CHANGE:` | **major** (but see the 0.x rule) | 1.4.0 → 2.0.0 |
| everything else | **no bump** | — |

**How it's computed.** release-please scans every commit since the last tag and applies the
**single highest** bump, once. Five `feat`s + three `fix`es landed together = **one** minor
bump — not eight releases.

**The 0.x rule (delryn is here now).** While the major version is `0`, the interface is
treated as unstable, so bumps shift down one level (via `bump-minor-pre-major: true`):

- `feat!:` / breaking → **minor** (0.3.0 → 0.4.0), *not* major
- `feat:` → minor · `fix:` → patch (via `bump-patch-for-minor-pre-major: false`)

Reaching **1.0.0 is a deliberate act** — release-please never auto-promotes; you cut it when
you decide delryn's interface is stable. After 1.0, breaking changes bump major as usual.

**One version drives releases — and it lives in git, not `Cargo.toml`.** delryn is released as a
single unit, so there is exactly one release version. release-please tracks it in
`.release-please-manifest.json` and stamps it into the `vX.Y.Z` tag, the GitHub Release, and
`CHANGELOG.md`. It does **not** touch any `Cargo.toml` or `Cargo.lock` — that's the whole point
of the `simple` release-type, and it's why `cargo build --locked` CI can never be broken by a
release. The crate `version` fields all inherit `[workspace.package].version` and, since delryn
is never published to a registry, nothing downstream reads them.

**The binary does not read them either — it is told the version at build time.** Three things
report a version to a human or a server: `delryn --version`, the crash log, and the HTTP
User-Agent. All three read `delryn::VERSION`, which prefers the `DELRYN_VERSION` environment
variable set during the release build (`release-build.yml` passes the tag with its leading `v`
stripped) and falls back to `CARGO_PKG_VERSION` for a source build, where there is no tag and
the manifest is the honest answer. So a released binary reports its release version even though
`Cargo.toml` still says something older — which is the point of not churning crate versions.

> Left alone, this drifts silently: the manifest was `0.1.0` while the manifest-tracked release
> version was `0.0.0`, and after the first release the crash log would have reported the wrong
> version on every bug report. If you add another consumer of the version, read
> `delryn::VERSION`, never `env!("CARGO_PKG_VERSION")`.

**Source of truth.** The open **release PR always shows the exact next version** release-please
picked — check it there. If it ever disagrees with this table, the fix is in
`release-please-config.json`, never a hand-edit.

---

## Changelog convention

`CHANGELOG.md` is **generated by release-please — never hand-edit released sections.** You
maintain it purely by writing good PR titles.

- **One section per version**, newest on top, each linked to its tag/Release and dated.
- **Entries come straight from PR titles**, grouped by type — so clean PR titles *are* the
  release notes:

  | PR-title type | Changelog group |
  | --- | --- |
  | `feat:` | **Features** |
  | `fix:` | **Bug Fixes** |
  | `perf:` | **Performance Improvements** |
  | breaking | flagged with a **⚠ BREAKING CHANGES** heading |

- **Non-releasing types generally don't appear** (`chore`/`ci`/`docs`/`refactor`/`test`/
  `build`/`style`) — another reason to label a user-visible change `fix`/`feat`, not `chore`.
- The **GitHub Release notes** are exactly that version's changelog section — release-please
  writes them when it creates the Release.

**Your job here is nothing** — write clean PR titles and the changelog writes itself.

---

## The manual / hotfix path (`release.yml`)

For a hotfix cut outside the normal flow, push a `v*` tag by hand:

```sh
git tag v0.2.1 && git push origin v0.2.1
```

`release.yml` handles it exactly like the automated path (below): it **refuses to publish
unless CI is already green on that exact commit**, creates the GitHub Release from the
matching `CHANGELOG.md` section (only if one doesn't already exist), and builds the binaries
via the shared build workflow.

---

## How the wiring works

delryn is a **virtual Cargo workspace** (root `Cargo.toml` has `[workspace]`, no `[package]`)
that builds **one binary** from ~10 internal path-dependency crates, none published to
crates.io — so it's released as **one unit**. release-please is configured with a single package
at path **`.`** (the repo root) using **`release-type: simple`**. Because the package path is
the repo root, release-please attributes **every** commit in **every** crate to it (release-please
path-splits commits by package directory, and `.` is the whole tree), so any `feat`/`fix`
anywhere bumps the one version. `simple` only rewrites `CHANGELOG.md` +
`.release-please-manifest.json` and cuts the tag/Release — it never runs cargo or touches
`Cargo.lock`. Config: `release-please-config.json` (package `.`, `release-type: simple`,
`include-component-in-tag: false`, `bump-minor-pre-major: true`,
`bump-patch-for-minor-pre-major: false`) + `.release-please-manifest.json` (`{".": "…"}`).

All workflows live in `.github/workflows/`:

| File | Role |
| --- | --- |
| `ci.yml` | Matrix build + `cargo test` on ubuntu/macos, plus `fmt --check` and `clippy -D warnings`. Job names (`test (ubuntu-latest)`, `test (macos-latest)`, `lint`) are the required status checks. |
| `pr-title.yml` | Validates the PR title is a Conventional Commit. Required check `Validate PR title`. |
| `release-please.yml` | The engine: maintains the release PR, then on merge tags `vX.Y.Z` + creates the GitHub Release. |
| `release-build.yml` | Reusable (`workflow_call`) build matrix — the single source of the target list; the one build path. |
| `release.yml` | Fires on any `v*` tag → gates on green CI → creates the Release if missing → calls `release-build.yml`. Handles **both** the automated (release-please) tag and manual hotfix tags. |

### The one thing that will bite you: the GitHub App token

Two facts about `GITHUB_TOKEN` drive the whole design:

1. **A PR opened by `GITHUB_TOKEN` does not trigger `pull_request` workflows.** So if
   release-please opened its release PR with `GITHUB_TOKEN`, CI + the PR-title check would
   never run on it, and under `enforce_admins` branch protection it would be **permanently
   unmergeable**.
2. **A tag created by `GITHUB_TOKEN` does not trigger any workflow** either — so the automated
   release tag wouldn't fire `release.yml`, and no binaries would build.

Both are solved by running release-please as a **GitHub App** instead of `GITHUB_TOKEN`:
`release-please.yml` mints a short-lived token with
[`actions/create-github-app-token`](https://github.com/actions/create-github-app-token) and
passes it to release-please. The App-authored release PR gets its checks (mergeable), and the
App-created `vX.Y.Z` tag **triggers `release.yml`**, which builds. One build path, no
double-build, nothing to double-check.

**The App needs three repository permissions:** **Contents: RW** (commits, tags, releases),
**Pull requests: RW** (the release PR), and **Issues: RW** (release-please tracks state with
`autorelease:*` labels, which go through the Issues API). Two repo secrets drive it:

- `RELEASE_PLZ_CLIENT_ID` — the App's Client ID *(legacy name from the prior release-plz setup; same App)*.
- `RELEASE_PLZ_PRIVATE_KEY` — the App's private-key `.pem` contents.

> **⚠ If you migrated from release-plz:** the App was set up with **Contents + Pull requests
> only**. release-please additionally needs **Issues: Read & write** for its labels — add it
> in the App's *Permissions & events*, then re-approve the installation. Without it,
> release-please may fail to open/label the release PR.

### libpdfium

delryn binds `libpdfium` at runtime. Without it EPUB/MOBI still read fine and only PDF is
unavailable, with the reader saying so. Search order:

1. `DELRYN_PDFIUM_DIR`
2. beside the running executable — what a release tarball provides
3. a sibling `../lib`, for a `bin/` + `lib/` install layout
4. `~/.local/lib/delryn`, then `<config>/lib`
5. the system library
6. **the copy embedded in the binary**, unpacked to `<config>/lib` on first use

**Release builds embed it.** Upstream ships no static build (every asset is a dynamic `.tgz`),
so linking PDFium in properly would mean building it from source with depot_tools. Instead
`release-build.yml` sets `DELRYN_PDFIUM_LIB` and `delryn-format/build.rs` compiles the verified
library into the executable. This exists for one common case: people copy `delryn` out of the
tarball onto their `PATH` and delete the folder, leaving the loose library behind — before, PDFs
then stopped working with no obvious cause. The embed is tried **last**, so an intact install
never pays the unpack and a deliberately-placed library still wins. Cost: ~7 MB of binary
(≈30 → ≈37 MB).

**Ordinary `cargo build` embeds nothing** — `DELRYN_PDFIUM_LIB` is unset, the constant is an
empty slice, and the search above still applies. See the source-build setup below.

> **If delryn is ever notarized** (hardened runtime), revisit this: a hardened process will
> refuse to `dlopen` an unsigned library unpacked at runtime. The loose bundled copy would need
> to be signed, or the unpacked one signed on extraction.

**The download** comes from [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries),
pinned to `chromium/7763` — the API level `pdfium-render 0.9.2` targets — and is verified against
`.github/pdfium-checksums.txt` before it is either embedded or bundled. Bump `PDFIUM_RELEASE`
**and regenerate the checksum file** in lockstep with any `pdfium-render` upgrade; the build
fails on a mismatch rather than shipping an unverified native library.

**In a source build** there is no tarball, so `cargo build` alone leaves PDF unavailable — this
is the usual cause of "libpdfium not found". Fetch the same pinned build once (swap
`mac-arm64` for `mac-x64` or `linux-x64`):

```sh
curl -sSfL -O "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7763/pdfium-mac-arm64.tgz"
shasum -a 256 -c <(grep ' pdfium-mac-arm64.tgz$' .github/pdfium-checksums.txt)
tar -xzf pdfium-mac-arm64.tgz lib/libpdfium.dylib
mkdir -p ~/.local/lib/delryn && cp lib/libpdfium.dylib ~/.local/lib/delryn/
```

Then point delryn at it permanently — this survives `cargo clean`, which wipes anything copied
into `target/`:

```sh
export DELRYN_PDFIUM_DIR="$HOME/.local/lib/delryn"
```

---

## Repo settings (one-time)

- **Merge:** squash-only, squash commit title = PR title, delete branch on merge.
- **Branch protection on `main`:** required checks `test (ubuntu-latest)`,
  `test (macos-latest)`, `lint`, `Validate PR title`; `enforce_admins: true`; force-push
  and deletion blocked.
- **Release-bot GitHub App:** Repository permissions **Contents: RW + Pull requests: RW +
  Issues: RW**, installed on this repo.
- **Secrets:** `RELEASE_PLZ_CLIENT_ID` + `RELEASE_PLZ_PRIVATE_KEY` (the release-bot App).
