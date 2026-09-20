# Background worker activity proof

Direct Playwright screenshots of the actual Buzz desktop renderer at source commit 89c83e872 (20 September 2026). The test uses the repository's mock bridge and observer-event fixtures; these are not screenshots of a production agent or a live relay session.

The regression exercises Pending → Running → Failed after parent completion, expansion of the failure details, process-exit retirement, preservation of completed calls, and isolation from another runtime slot. See desktop/tests/e2e/observer-feed-screenshots.spec.ts in PR #74.

Images are original captures without compositing or content edits.
