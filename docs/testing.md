# AIcleaner Testing Guide

This project uses four separate validation layers. Keep them separate so routine regressions stay cheap, while real-model experiments remain explicit and traceable.

## 1. Stable Regression Tests

Use these by default during normal development. They must stay local, deterministic, and offline.

| Scope | Command | When to run |
| --- | --- | --- |
| Frontend unit tests | `npm run test:frontend` | Frontend helper, model, or component changes |
| Frontend full check | `npm run check:frontend` | Any meaningful frontend change |
| Rust library tests | `npm run test:rust` | Backend, runtime, persistence, prompt/tool contract changes |
| Full local check | `npm run check:all` | Cross-cutting changes before handoff |

`npm test` remains an alias for Vitest. Do not add real-model tests to `npm test`, `npm run check:frontend`, or `npm run check:all`.

## 2. Real-Folder Smoke

Use this only when the user explicitly wants a real directory and real model request. It validates that the organizer classification path works end to end on a small sample.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\testing\run-classification-smoke.ps1 `
  -RealFolder "E:\Download" `
  -SummaryStrategy filename_only `
  -MaxItems 24 `
  -RealBatchSize 8 `
  -RealConcurrency 2
```

The script resolves model configuration from AIcleaner settings and Windows Credential Manager unless `-Endpoint`, `-Model`, and `-ApiKey` are provided explicitly. API keys must never be printed or written to result files.

Outputs are written under `test-runs/`:

- `<timestamp>-smoke.jsonl`
- `<timestamp>-smoke-summary.md`
- `<timestamp>-smoke-raw.log`

Smoke success means the command exits successfully, the model assigns all sampled items, and the summary records no missing assignments.

## 3. Parameter And Concurrency Experiments

Use experiments when comparing batch sizes, request concurrency, or summary strategies. These are not simple pass/fail tests; they produce comparable measurements.

Capacity sweep:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\testing\run-organizer-experiment.ps1 `
  -Mode capacity `
  -RealFolder "E:\Download" `
  -BatchSizes "4,8,12,16,24" `
  -Repeats 1 `
  -SummaryStrategy filename_only
```

Concurrency sweep:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\testing\run-organizer-experiment.ps1 `
  -Mode concurrency `
  -RealFolder "E:\Download" `
  -BatchSize 10 `
  -MaxItems 240 `
  -ConcurrencyValues "1,2,4,8" `
  -SummaryStrategy filename_only
```

The experiment runner reuses the ignored Rust tests:

- `real_folder_single_batch_capacity_sweep_with_real_model`
- `real_folder_small_batch_concurrency_sweep_with_real_model`

Outputs are written under `test-runs/`:

- `<timestamp>-<mode>.jsonl`
- `<timestamp>-<mode>-summary.md`
- `<timestamp>-<mode>-raw.log`

Each JSONL result row includes run identity, mode, root path, endpoint/model, summary strategy, batch/concurrency parameters, success state, timing, assignment counts, token usage, and error text. Treat large error rates, missing assignments, duplicate assignments, or unknown assignments as experiment failures even if some requests succeed.

## 4. Diagnostics Analysis

Diagnostics analysis is read-only and must not call the model. Use it when checking the latest organizer run, timing breakdowns, payload sizes, raw response sizes, or preserved model/provider errors.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\testing\analyze-diagnostics.ps1
```

Useful overrides:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\testing\analyze-diagnostics.ps1 `
  -LogsDir "E:\Cache\AIcleaner\logs" `
  -Limit 30
```

The analyzer scans the newest diagnostics files and chooses the first file containing organizer events. Outputs are written under `test-runs/` as JSONL and Markdown summary files.

## Failure And Reporting Rules

- Preserve original error text in script output, JSONL, Markdown summary, and diagnostics whenever practical.
- Do not silently fall back from a failing real-model run to an offline path.
- Keep smoke/experiment stdout metrics separate from backend diagnostics timings.
- Report whether a number came from smoke stdout, experiment JSONL, or diagnostics JSONL.
- Use `-AllRealItems` only intentionally; large folders should stay chunked with explicit batch size and concurrency.
