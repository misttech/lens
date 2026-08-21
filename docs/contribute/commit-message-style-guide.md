# Commit message style guide

Lens uses plain Git and GitHub (`github.com/misttech/lens`). Good commit messages
explain *why* a change was made, not just what changed. Follow these guidelines.

## Subject line

* **Start with one or more component tags** in the form `[parent][component]`,
  then the summary. See [Component tags](#component-tags) below.
* Use the [imperative mood](https://en.wikipedia.org/wiki/Imperative_mood):
  "Add elision markers", not "Added elision markers".
* Keep it under ~50 characters when you can (not a hard limit).
* Capitalize the word after the tags; no trailing period.

## Component tags {#component-tags}

Prefix every subject with the area(s) it touches, in square brackets. Use a
second tag to narrow scope (`[parent][component]`). Tags are lowercase and match
the tree — a module or directory name:

| tag | area |
|---|---|
| `[cli]` | `src/cli.rs` (arg splitting, subcommands) |
| `[resolve]` | `src/resolve.rs` (PATH resolution, passthrough detection) |
| `[executor]` | `src/executor.rs` (spawn, capture, exit codes) |
| `[store]` | `src/store.rs` (content-addressed run store) |
| `[pipeline]` | `src/pipeline/` (add a stage tag, e.g. `[pipeline][budget]`) |
| `[adapters]` | `src/adapters/` (add a name, e.g. `[adapters][git]`) |
| `[render]` | `src/render.rs` (views, levels, elision markers) |
| `[config]` | `src/config/` (lens resolution and merge) |
| `[log]` | `src/log.rs`, `lens stats`, `lens logs` |
| `[plot]` | `src/plot.rs` (pipeline visualization) |
| `[tokens]` | `src/tokens.rs` (token estimation) |
| `[platform]` | `src/platform.rs` (the OS boundary) |
| `[bench]` | `benches/`, `bench/` (add `[bench][micro]` or `[bench][retention]`) |
| `[tests]` | `tests/`, fixtures, goldens |
| `[docs]` | `README.md`, `docs/` |
| `[build]` | `Cargo.toml`, `Makefile`, `scripts/`, CI |
| `[repo]` or `[git]` | repo/git meta files: `.gitignore`, `.gitattributes`, `LICENSE`, `NOTICE`, editor config |

Examples:

```none
[executor] Propagate signal deaths as 128 + signum
[pipeline][classify] Force-keep stderr tail when the child failed
[adapters][git] Preserve @@ line numbers across elided hunks
[bench][retention] Add the last-line-of-5000 truncation trap
[repo] Ignore /out build output
```

## Body

Separate the subject from the body with a blank line, then describe the change
in more detail. Make the *reason and intention* clear — the diff already shows
what changed.

```none
[pipeline][classify] Force-keep stderr tail when the child failed

A failing command whose filtered output shows no failure is the worst
bug this tool can have: the agent reads a clean view and concludes the
command succeeded. So a non-zero exit raises the floor — if no block
classified as Error or Failure, the tail of stderr is kept regardless
of budget.
```

* Wrap body lines at ~72 characters.
* The body is optional if the subject fully explains the change.
* Don't reference private URLs, individuals, secrets, or relative points in time.

### Two project-specific rules

**A commit touching an invariant says which one.** The invariants listed in
`AGENTS.md` are correctness properties, not tradeoffs. Name the one a change
upholds or fixes against:

```none
Line addressability: eliding the whole region with a marker keeps
every file:line reference correct, where renumbering would silently
break jump-to-line.
```

**A `Test:` footer is required under `src/pipeline/` and `src/adapters/`.** Those
are the paths where output gets deleted, and "the tests pass" is not a test plan.
Record the fixture or the real command that exercised the change, with its result.

## Referencing issues (optional)

If a change relates to a GitHub issue, mention it in the body or footer using
GitHub's own keywords:

```none
Fixes #123
```

* Use `Fixes #<n>` / `Closes #<n>` to auto-close an issue on merge, or
  `Refs #<n>` to link without closing.
* Only when a relevant issue exists — do not invent issue numbers.

## Tests

Describe how you verified the change, either in the body or with a `Test:` line:

```none
Test: make test
Test: lens git diff on a 5k-line diff; marker carried the handle and
      show --level 3 diffed clean against `git diff`
```

Documentation-only or trivial changes don't need one.

## Pull requests

`main` is linear and nothing lands on it except through a pull request, **rebase
merged**. Squash and merge commits are both disabled in the repository settings,
so a branch arrives on `main` as the commits it contains.

That has one consequence that governs how you work on a branch:

* **Every commit is permanent history.** Nothing is collapsed at merge, so no
  commit gets to be a working note. Each one is a coherent change with a message
  that follows the rules above — and a branch that accumulated "wip" and "fix
  typo" commits gets cleaned up with `git rebase -i` before review, not left for
  the merge to hide.
* **Every commit should build and pass tests.** Rebase merge puts each one on
  `main` individually, which is where `git bisect` will land on it.
* Update a branch with `git rebase main`, never `git merge main`. A merge commit
  cannot land here — the repository requires linear history.

The PR title and description are still worth writing carefully: they are what a
reviewer reads first, and they are the only place the branch is described as a
whole.
