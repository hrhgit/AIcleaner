---
name: prebuild-solution-research
description: Research reusable options before implementing a new capability.
---

# Prebuild Solution Research

## Goal
Decide `reuse-first` vs `build-custom` before coding.

## Trigger
Use only for new feature/module/system capability work. Skip tiny fixes/copy edits/visual tweaks. If unclear, assume new feature and note it.

## Compact Workflow
1. Classify request.
2. GitHub search and keep `2-3` candidates.
3. Evidence levels: `L1` GitHub-only; `L2` + official docs (high risk); `L3` + registry (still unclear/conflicting).
4. Score with `scripts/score_candidates.py` and output report.

High-risk: license missing/ambiguous, last commit `>180` days, or release info missing/conflicting.

## Default Decisions
Scorer interface unchanged. Thresholds unchanged: pass `70`; hard gates include maintenance within 18 months.
If any gate-pass candidate scores `>=70` => `reuse-first`; else `build-custom`.

## Output Contract
Default language: Chinese.
Report has exactly 5 sections: `结论`, `候选对比`, `关键证据`, `风险与回滚`, `下一步执行`.
Table columns: `Candidate | Score | Gate | EvidenceLevel | Key Risk | Links`.
Every key claim must include links.

## Resources
- `references/query-patterns.md`
- `references/maturity-rubric.md`
- `references/report-template.md`
- `scripts/score_candidates.py`
