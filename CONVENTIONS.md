# CONVENTIONS

Standing rules for Claude Code, Codex, and any agent executing work in this repo. Every handoff assumes these. Read before starting. Each rule maps to a real failure that has happened.

## Scope
- A handoff is one build session. Everything in it is in scope now.
- There is no phase two. If you write "later," "future milestone," "separate effort," or "out of scope for now" about something the handoff lists, stop and build it.
- Deliver the full list. Partial completion is not completion.

## Named choices are requirements
- Every named library, tool, API, model, or technique in a handoff was chosen deliberately. It is a requirement, not an example.
- Do not substitute your own preferred alternative, even if you think it is better.
- If you believe a named choice is genuinely wrong, say so and ask before changing it. Never silently substitute.

## Do not manufacture blockers
- Before treating something as a blocker (credential, data source, dependency, model weight), check what is actually in the repo and the provided materials.
- Assume the simplest version of an access problem, not the hardest. Provided data is provided; do not invent an acquisition or legal dependency.
- If something is genuinely missing, ask. Do not descope around an assumed obstacle.

## Done means verified
- Done means verified end to end against the deliverable's acceptance criterion, observable or inspectable, not "it compiles" or "a unit test passed."
- State which criterion you verified and how.
- Where deliverables form a chain, verify each before building the next. A later step built on an unverified earlier one is how whole builds end up non-functional.

## Never fake the UI or the data
- Never render fabricated or placeholder values as if real.
- Fixture, inferred, and low-confidence data are labeled as such, in the UI and in the data.
- A count or status that reads as more complete or more live than it is, is a bug.

## Coordinate and review
- When two agents share a task, post intent before parallel work, and review the other's diff against the acceptance criteria before either reports done.
- Record load-bearing decisions to the harness so they survive across sessions. Do not assume the harness will replay intent on its own; the handoff carries it.

## Reporting
- Report status as a scannable list: deliverable, acceptance criterion, verified or not, how.
- Lead with what is not done or not verified.
- Skip the long prose recap of what you did. A concrete status list against the deliverables gets read; a verbose narration gets skimmed.

## Writing and code
- Concrete over prose: file paths, function signatures, data shapes, exact names.
- No em dashes. No time estimates. No filler or rationale padding.
- Confirm names and contracts against the actual source before writing against them; do not assume field or function names from a handoff.
