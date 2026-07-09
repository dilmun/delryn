# Releasing delryn

delryn uses an automated pipeline where the maintainer makes **exactly one decision**:

> **"Release now?" → merge the standing release PR.**

Everything else — the version bump, `CHANGELOG.md`, the `vX.Y.Z` tag, the GitHub
Release, and the per-platform binaries — is automatic. You never choose a version
number, write release notes, or push a tag by hand.

The engine is [**release-plz**](https://release-plz.dev) (not release-please): delryn is
a virtual Cargo workspace where every crate inherits one version from
`[workspace.package].version`, and release-plz bumps that inherited version natively.

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
                             ▼   release-plz keeps ONE PR current    │
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
                    R          ← branch  release-plz--branches--main
                                 ONE commit: bump Cargo.toml + write CHANGELOG
                    │
                    └──► PR:  [release-plz--branches--main] ──merge into──► [main]
```

You never touch that branch. You only ever click **Merge** on the PR.

### How the bot branch gets created (you didn't — a workflow did)

```
   you merge a PR to main
          │
          ▼
   GitHub runs  .github/workflows/release-plz.yml   (on: push to main)
          │
          ▼
   job "release-pr":  release-plz release-pr   🔑 using the GitHub App token
          │
          ▼
   the App (Contents: write + Pull requests: write) does the git for you:
      • git branch  release-plz--branches--main
      • git commit  (version bump + CHANGELOG)
      • open / update the PR
```

The workflow is the trigger; the App is the identity that runs `git`. That's why the
release PR is authored by `dilmun-release-bot`, not you.

---

## Day-to-day: landing a change

`main` is protected (required checks + `enforce_admins`), so you **cannot push to it
directly** and you **never merge into it locally** — a local merge would be rejected and
would skip the PR-title → squash → release-plz chain. Every change goes through a branch
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
| PR | `gh pr create --title "feat: …"` | The **PR title becomes the squash commit** — the one Conventional Commit release-plz reads. |
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
these only for docs-/CI-only changes with no code to break (like this PR).

---

## Commit & PR-title convention

Because we squash-merge, the **PR title is the commit standard** — it's the single commit
that lands on `main`, the Conventional Commit release-plz parses, and the changelog line
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

\* release-plz never bumps on `perf`/`refactor`, and "user-visible" isn't machine-detectable
for a binary. **If users would notice the change, label it `fix` or `feat`** — that's the
only way it reaches a release and the changelog. See [Release version convention](#release-version-convention-semver)
for the exact bump rules.

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
(Optional personal helper: `git config commit.template .gitmessage`.)

---

## Release version convention (SemVer)

delryn follows [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`. release-plz
computes the next version automatically from the Conventional Commit **types** merged since
the last release tag — you never pick a number.

| PR-title type | Bump | Example |
| --- | --- | --- |
| `fix:` | **patch** | 0.3.1 → 0.3.2 |
| `feat:` | **minor** | 0.3.1 → 0.4.0 |
| `feat!:` / `BREAKING CHANGE:` | **major** (but see the 0.x rule) | 1.4.0 → 2.0.0 |
| everything else | **no bump** | — |

**How it's computed.** release-plz scans every commit since the last tag and applies the
**single highest** bump, once. Five `feat`s + three `fix`es landed together = **one** minor
bump — not eight releases.

**The 0.x rule (delryn is here now).** While the major version is `0`, the interface is
treated as unstable, so bumps shift down one level:

- `feat!:` / breaking → **minor** (0.3.0 → 0.4.0), *not* major
- `feat:` → minor · `fix:` → patch (unchanged)

Reaching **1.0.0 is a deliberate act** — release-plz never auto-promotes; you cut it when you
decide delryn's interface is stable. After 1.0, breaking changes bump major as usual.

**One version for the whole workspace.** All crates inherit `[workspace.package].version`, so
there's a single delryn version and a single tag `vX.Y.Z`. A `feat` in *any* crate (e.g.
`crates/delryn-render`) bumps that one version; `release-plz.toml`'s `changelog_include` folds
those commits into delryn's single changelog.

**Source of truth.** The open **release PR always shows the exact next version** release-plz
picked — check it there. If it ever disagrees with this table, the fix is in
`release-plz.toml`, never a hand-edit.

> `perf`/`refactor` do **not** release, and "user-visible" isn't machine-detectable for a
> binary — so **if users would notice the change, label it `fix` or `feat`.**

---

## Changelog convention

`CHANGELOG.md` is **generated by release-plz — never hand-edit released sections.** It follows
[Keep a Changelog](https://keepachangelog.com) + SemVer, and you maintain it purely by writing
good PR titles.

- **One section per version**, newest on top, each linked to its tag/Release and dated:
  `## [0.4.0](…/releases/tag/v0.4.0) - 2026-08-01`.
- **Entries come straight from PR titles**, grouped by type — so clean PR titles *are* the
  release notes:

  | PR-title type | Changelog group |
  | --- | --- |
  | `feat:` | **Added** |
  | `fix:` | **Fixed** |
  | breaking | listed in its group, flagged as breaking |

- **Scope becomes a prefix:** `feat(reader): remember position` → `- *(reader)* remember position`.
- **Non-releasing types generally don't appear** (`chore`/`ci`/`docs`/`refactor`/`test`/
  `build`/`style`) — another reason to label a user-visible change `fix`/`feat`, not `chore`.
- The **`[Unreleased]`** heading mirrors whatever is sitting in the open release PR.
- The GitHub Release's notes are exactly that version's section
  (`git_release_body = "{{ changelog }}"` in `release-plz.toml`).

**Your job here is nothing** — write clean PR titles and the changelog writes itself.

---

## The manual / hotfix fallback (`release.yml`)

For the rare case where you must cut a release outside the normal flow (e.g. a hotfix
tag), push a `v*` tag by hand:

```sh
git tag v0.2.1 && git push origin v0.2.1
```

`release.yml` then **refuses to publish unless CI is already green on that exact commit**
(`require-green-ci` polls the Actions API), creates the GitHub Release from the matching
`CHANGELOG.md` section (falling back to auto-generated notes), and builds the binaries via
the same shared build workflow.

---

## How the wiring works (and two things that will bite you if changed)

All workflows live in `.github/workflows/`:

| File                | Role |
| ------------------- | ---- |
| `ci.yml`            | Matrix build + `cargo test` on ubuntu/macos, plus `fmt --check` and `clippy -D warnings`. Job names (`test (ubuntu-latest)`, `test (macos-latest)`, `lint`) are the required status checks. |
| `pr-title.yml`      | Validates the PR title is a Conventional Commit. Required check `Validate PR title`. |
| `release-plz.yml`   | The engine: maintains the release PR, then tags/releases/builds on merge. |
| `release-build.yml` | Reusable (`workflow_call`) build matrix — the single source of the target list; shared by both release paths. |
| `release.yml`       | Manual `v*`-tag fallback. |

### 1. The release PR is opened by a GitHub App, not `GITHUB_TOKEN`

A PR opened by the default `GITHUB_TOKEN` **does not trigger `pull_request` workflows**
(GitHub prevents workflow recursion). So if release-plz opened its PR with `GITHUB_TOKEN`,
CI and the PR-title check would never run on it — and with `enforce_admins: true` on
branch protection, that PR could **never be merged**.

The fix is to open the PR as a **non-`GITHUB_TOKEN` identity**. We use a **GitHub App**
(the "release bot") rather than a PAT: a PAT is either forced to expire (fine-grained) or
grants access to *all* your repos (classic), whereas the App is repo-scoped, minimally
permissioned, mints a short-lived token per run, and doesn't expire — so it scales to
multiple repos with nothing to rotate.

The `release-pr` job mints an installation token with
[`actions/create-github-app-token`](https://github.com/actions/create-github-app-token)
and hands it to release-plz. Two repo secrets drive it (set once):

- `RELEASE_PLZ_APP_ID` — the App's numeric ID.
- `RELEASE_PLZ_PRIVATE_KEY` — the App's private-key `.pem` contents.

**One-time App setup:** create a GitHub App (no webhook) with Repository permissions
**Contents: RW** + **Pull requests: RW** — release-plz's documented minimal set; it derives
versions from git tags (not PR labels), so it never touches the Issues/labels API. Generate
a private key, install the App on this repo, then set the secrets below.

```sh
gh secret set RELEASE_PLZ_APP_ID     --repo dilmun/delryn --body "<app-id>"
gh secret set RELEASE_PLZ_PRIVATE_KEY --repo dilmun/delryn < app-private-key.pem
```

### 2. The tag/release job uses `GITHUB_TOKEN` *on purpose*

Tags created with `GITHUB_TOKEN` also don't trigger further workflows. We rely on that:
the `release` job creates the tag/release with `GITHUB_TOKEN` so it does **not** re-trigger
`release.yml` (`on: push: tags`) and cause a double build. Instead, the binary `build` job
lives inside `release-plz.yml`, gated on `needs.release.outputs.releases_created == 'true'`.

### libpdfium in the tarballs

delryn binds `libpdfium` at runtime from beside the executable. `release-build.yml`
downloads the matching `libpdfium` from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (pinned to
`chromium/7763`, the API level `pdfium-render 0.9.2` targets) and ships it inside each
tarball, so PDFs work on download with no extra install. Bump `PDFIUM_RELEASE` in lockstep
with any `pdfium-render` upgrade.

---

## Repo settings (one-time)

- **Merge:** squash-only, squash commit title = PR title, delete branch on merge.
- **Branch protection on `main`:** required checks `test (ubuntu-latest)`,
  `test (macos-latest)`, `lint`, `Validate PR title`; `enforce_admins: true`; force-push
  and deletion blocked. (Add the `Validate PR title` context only *after* its first green
  run, or it wedges every PR waiting on a check that never reported.)
- **Secrets:** `RELEASE_PLZ_APP_ID` + `RELEASE_PLZ_PRIVATE_KEY` (the release-bot App; see above).
