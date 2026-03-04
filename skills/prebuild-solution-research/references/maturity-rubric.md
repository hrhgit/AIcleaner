# Maturity Rubric (Compact)

Scoring implementation source of truth: `scripts/score_candidates.py`.
This file is a concise policy mirror; do not diverge from script constants.

## Hard Gates (all required)
1. Repository is not archived.
2. License is clear and usable.
3. Maintenance signal exists within 18 months.

If any gate fails: `gate_failed`.

## Weighted Score (100)
| Dimension | Weight |
|---|---:|
| Maintenance activity | 25 |
| Community adoption | 20 |
| Documentation completeness | 15 |
| Release stability | 15 |
| License availability | 10 |
| Engineering quality signals | 10 |
| Task fit | 5 |

## Mature Threshold
- `mature_candidate`: gates pass and total score `>=70`
- `candidate_with_risk`: gates pass and total score `<70`
- `gate_failed`: any hard gate fails

## Normalized Inputs
- `docs_score`: 0-10
- `quality_signals`: 0-10
- `fit_score`: 0-10

## Key Constants
- pass score: `70`
- maintenance hard-gate window: `18 months` (`548` days in script)
