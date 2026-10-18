# Questions from batch mode

Questions and notes for the maintainer, written while working through `phase-1-finish.md`. Stop conditions (a migration, an export-format change, a dependency with a new licence, anything that deletes user data) are listed here, and the slice that hit one was skipped.

## Notes (no stop condition)

1. **#70 is on `phase-1` as a cherry-pick (#71).** The brief restyles the Keyboard Shortcuts and About dialogs, which only exist in #70 (open against `main`). The commit was cherry-picked unchanged. Close #70 once `phase-1` reaches `main`, or merge #70 first and rebase `phase-1`.

2. **A damaged stored setting stops the app with no way on** (ADR 0050 row 37). If a value in the `settings` table is out of range (a damaged or hand-edited database), Rappel shows "Rappel Could Not Start" with the cut error and nothing else. Resetting the value to its default would overwrite user data, so batch mode left it. Options: offer "Reset This Setting" on that page (one row, with a backup taken first), or keep refusing and point to Restore a backup. Which do you want?
3. **Restoring a backup still replaces a waiting restore copy** (ADR 0007, noted in ADR 0050 row 35). This was already so before batch mode. The copy is the user's data. Should a restore set it aside (like Start Fresh now does) instead?
4. **Open PRs against `main` that overlap `phase-1`.** #69 (release 0.2.0-alpha.4) bumps the same version lines as 0.2.0-beta.1 on `phase-1`: close it, or merge it first and resolve the CHANGELOG heading. #56 (disable Reset Progress for new cards) touches the card rows, the review store and ADR 0028, which `phase-1` also changed: rebase it after `phase-1` reaches `main`. #70 see note 1. #35, #32 and #25 were not touched.
5. **Tags are never deleted** (ADR 0029). A typo tag that was added and then undone stays in the tag suggestions and the tag filter. Hiding tags no card uses from the suggestions is a small phase 2 change; deleting unused tags would be a data change and needs your decision.
6. **The draft PR `phase-1` → `main`.** The brief asks for it, while batch mode says never to target `main`. Batch mode opened it as a **draft** only. The guard blocks merging it, and nothing was pushed to `main`. Merging it is yours.
