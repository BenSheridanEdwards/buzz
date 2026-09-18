<!--
Fleet Buzz pull request body, carried over from F.L.E.E.T. Guidance lives in
these HTML comments; GitHub hides them when it renders the PR, so leave them in
or delete them. Keep the four `#` headings, in this order: reviewers and the
review agents look for them by name.

Title: Conventional Commits, `type(scope): summary`. No `[agent]`-style prefix.

Write for a reviewer who has five minutes. The first screen must answer three
questions: what can someone do now, what did the issue ask for, and did the
diff do that and only that. Long logs and transcripts go inside a <details>
block under the one-line result they support. Describe behaviour and named
symbols, not line numbers. Line numbers go stale before the review ends.
-->

# Why does this feature exist?

<!--
Lead with the outcome for a user or operator, in plain language. A PR was
rejected as "too technical, misses the point" for opening on internals.
Before and After describe behaviour, not implementation.
Issue: link the issue or spec that asked for this change, as `Closes #N` or
`part of #N`. The reviewer grades the diff against it.
-->

- Outcome:
- Before:
- After:
- Issue:

# What changed?

<!--
Summary: what the diff does at product-behaviour level, in one or two
sentences.
Out of scope: what this PR deliberately does not do, so the reviewer knows the
boundary and does not grade a missing piece as a defect.
-->

- Summary:
- Out of scope:

## Acceptance criteria and evidence

<!--
One checkbox per acceptance criterion from the issue. Each line names the
evidence that closes it: a named test, a screenshot from the proof section, or
a command with its result. A criterion with no evidence stays unchecked. A
spec review reads this section to find missing work, so it must be complete.
Format: `- [x] <criterion>. Evidence: <test title | screenshot | command and result>`
-->

- [ ] Criterion:

## Behaviour changes

<!--
One line per intentional change: Given ..., when ..., then ...
Write `None` when existing behaviour is preserved.
-->

- Behaviour change:

## Not asked for

<!--
Anything in the diff the issue did not ask for: a refactor, a rename, an extra
fix, a dependency bump. Name it and say why it rides in this PR. A reviewer
looks here for scope creep, so an honest list is faster than a hunt.
Write `None` when the diff only does what the issue asked.
-->

- Extra change:

## Review guide

<!--
Where to start reading and why, riskiest change first. Name the file or
symbol a reviewer must understand before the rest makes sense. Name every
decision only a human can make: a judgement call, a design choice, a
permission, a production setting. Write `None` when there is none.
-->

- Start here:
- Riskiest change:
- Needs a human decision:

## Test support changes

<!--
Every changed test helper, fixture, mock bridge or E2E spec (desktop/tests,
crates/buzz-test-client, mobile/test), by exact repository path, and why it
changed. Write `None` when none changed.
-->

- Changed support file:

# Behavioural Proof (with video and screenshots)

<!--
Match the proof to the risk. Screenshots for every visible state the change
affects, captured from the real app at the reviewed commit and embedded
inline. Video only when a still cannot carry it. Behaviour tests by exact test
title. Write `Not applicable` with the technical reason when the change has no
rendered or behavioural surface.
-->

- Screenshots:
- Video:
- Behaviour tests:

# Verification Summary

<!--
Every claim pairs a command with its result, so a reviewer can rerun it. A
command with no result is a claim, not evidence. Separate failures this PR
introduced from failures already on the base branch, and say how you proved
they are pre-existing. Name the owner of anything skipped.
-->

- Definition of Done:
- Commands run:
- Results:
- Unrelated failures:
- Known risks or skipped checks:

# PR Proof Law

- [ ] Every screenshot is a direct capture of the real changed UI or runtime at the reviewed commit, not generated, reconstructed, composited, selectively cropped, or a proof card.
- [ ] The PR body has no bare screenshot links, local paths, relative paths, or proof placeholders.
- [ ] Every claim in Verification Summary pairs a command with its result.
- [ ] The title is Conventional Commits (`type(scope): summary`) and every commit on the branch is too.
