# 极简决策报告模板（5段）

## 1) 结论
- Recommendation: `reuse-first` 或 `build-custom`
- Recommended candidate: `<name or N/A>`
- One-line reason: `<why>`

## 2) 候选对比
| Candidate | Score | Gate | EvidenceLevel | Key Risk | Links |
|---|---:|---|---|---|---|
| <A> | <score> | <pass/fail> | <L1/L2/L3> | <risk> | <urls> |
| <B> | <score> | <pass/fail> | <L1/L2/L3> | <risk> | <urls> |

## 3) 关键证据
- GitHub: <repo/issue/discussion links>
- Official docs (if used): <links>
- Registry (if used): <links>

## 4) 风险与回滚
- Risk 1: <risk>
- Risk 2: <risk>
- Rollback trigger: <condition>
- Rollback action: <how to revert>

## 5) 下一步执行
1. <step 1>
2. <step 2>
3. <step 3>

---
Rules:
- Keep reasons short (one sentence per candidate).
- Every key claim must have at least one link.
- If no mature candidate: explicitly state “no mature reusable solution found” and give custom-build path.
