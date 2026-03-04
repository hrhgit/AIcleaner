# Query Patterns (Compact)

## Source Priority
1. GitHub repositories/issues/discussions (default).
2. Official docs (only when escalation is required).
3. Official registry page (only for unresolved risk/conflict).

## Candidate Count
- Shortlist `min=2`, `max=3`.
- If more than 3 are found, keep the best-evidenced 3.

## Query Construction
Use: capability keyword + stack keyword + constraint keyword.
Prefer qualifiers: `archived:false`, `stars:>200`, `pushed:>=YYYY-MM-DD`, `language:<lang>`.

## Minimal Query Templates
1. `"<capability>" "<language|runtime>" "<framework>" stars:>200 archived:false`
2. `"<capability>" "production" "<stack>" pushed:>=<date> archived:false`
3. `"<candidate-name>" issue discussion roadmap`

## Layered Evidence Policy
- `L1` (default, GitHub-only): repo URL, stars/forks snapshot, last commit days, license, release recency.
- `L2` (high risk): add official docs URL and compatibility notes.
- `L3` (still uncertain/conflict): add registry URL and package release/maintenance confirmation.

## Escalation Triggers
Escalate from L1 when any condition is true:
- license is missing/ambiguous
- last commit age `>180` days
- release data is missing or conflicting

## Hard Filters
Exclude or fail if:
- archived repository
- license unclear/non-usable
- no maintenance signal within 18 months
