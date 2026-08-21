<!--
Title: [component] Imperative summary — same form as a commit subject. See
docs/contribute/commit-message-style-guide.md for the tag list.

Delete any section that does not apply. An honestly deleted section is more
useful than a heading with nothing under it.

`main` is rebase merged, so every commit lands as-is. Clean the branch up before
review rather than relying on the merge to hide anything.
-->

## Summary

<!--
One paragraph: what this changes and why it needed changing. If the branch is
several commits, say whether to review it as one change or commit by commit.

Lead with the problem, then the fix. "X was possible because Y; now Z" lets a
reviewer disagree with the premise rather than only with the code.
-->

## Test plan

<!--
Commands you actually ran, with results. "Tests pass" is not checkable; "74
unit, 35 integration, clippy clean" is. Name what the new tests pin down.
-->

```
make check test
```

## Invariants

<!-- LENS.md §2. Tick what this change touches; delete the rest. -->

- [ ] **Exit code fidelity** — the child's code is propagated unchanged.
- [ ] **Stream separation** — stdout and stderr are never merged.
- [ ] **Elision is announced** — anything removed leaves a marker carrying a handle.
- [ ] **Nothing is unrecoverable** — the full output still reaches the store.
- [ ] **Passthrough on doubt** — a Lens failure never becomes the user's failure.
- [ ] **Line addressability** — `file:line` references still resolve; no renumbering.
- [ ] **Logging** — never on the child's streams, never able to fail the run.

## Changed behavior

<!--
Anything an existing user or script must do differently: a changed default, a
new flag, a different view at the same budget, a log or meta.json field that
changed shape. The log schema is a published contract — a jq pipeline reads it.

Also worth a line: behavior that is not breaking but is not what a reader would
assume.
-->

## Note for reviewers

<!--
Where a mistake here is expensive, and anything you found but deliberately did
not fix. Saying so keeps it a decision rather than a surprise for whoever reads
the code next.
-->

---

Bug: <!-- Fixes #n / Closes #n to auto-close, Refs #n to link. Delete if none. -->
Test: <!-- one line, when the test plan above is not self-evident -->
