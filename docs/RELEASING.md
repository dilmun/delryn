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

## The primary path (99% of releases)

1. **Land work as Conventional-Commit PRs.** Branch → PR → **squash-merge**. The repo is
   configured so the squash commit subject *is the PR title*, and the PR title must be a
   valid Conventional Commit (enforced by the required `Validate PR title` check). That
   single line is what release-plz reads.
2. **release-plz maintains a standing release PR.** On every push to `main` it opens/updates
   one PR titled like `chore: release 0.2.0` that bumps `[workspace.package].version` +
   `Cargo.lock` and rewrites `CHANGELOG.md` from the commits since the last release.
3. **Merge that PR when you want to ship.** Merging triggers `release-plz.yml` to:
   tag `vX.Y.Z` → create the GitHub Release (notes from the changelog) → build and attach
   the binaries. Done.

Until you're ready, just keep landing PRs. The release PR keeps growing.

---

## Versioning (what bumps the version)

Determined mechanically from Conventional Commit types:

| Commit type                         | Bump                              |
| ----------------------------------- | --------------------------------- |
| `feat:`                             | **minor** (0.**x**.0)             |
| `fix:`                              | **patch** (0.1.**x**)             |
| `!` / `BREAKING CHANGE:`            | **major** — but **minor while 0.x** |
| everything else (`docs`, `chore`, `refactor`, `perf`, `test`, `build`, `ci`, `style`) | **no release** |

> **Rule of thumb:** release-plz does **not** bump on `perf`/`refactor`, and
> "externally observable" isn't machine-detectable for a binary. **If a change is
> user-visible, label it `fix` or `feat`.** That's the only reliable way to get it into a
> release and the changelog.

Because the whole workspace shares one version, a `feat` under any crate (e.g.
`crates/delryn-render`) bumps the single delryn version; `release-plz.toml`'s
`changelog_include` folds those commits into delryn's one changelog.

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
