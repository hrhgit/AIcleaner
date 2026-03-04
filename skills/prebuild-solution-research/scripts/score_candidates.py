#!/usr/bin/env python3
"""
Deterministic scorer for reusable solution candidates.

Usage:
  python score_candidates.py --input candidates.json [--output scored.json] [--pretty]

Input JSON formats:
  1) [{...}, {...}]
  2) {"candidates": [{...}, {...}]}

Expected candidate fields:
  - name
  - stars
  - last_commit_days
  - license
  - release_recency
  - docs_score        (0-10)
  - quality_signals   (0-10)
  - fit_score         (0-10)
Optional:
  - archived          (bool)
  - source_url
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any, Dict, List, Tuple

PASS_SCORE_DEFAULT = 70.0
MAINTENANCE_GATE_DAYS = 548

CLEAR_LICENSES = {
    "MIT",
    "APACHE-2.0",
    "BSD-2-CLAUSE",
    "BSD-3-CLAUSE",
    "ISC",
    "MPL-2.0",
    "EPL-2.0",
    "LGPL-2.1",
    "LGPL-3.0",
    "GPL-2.0",
    "GPL-3.0",
    "AGPL-3.0",
    "UNLICENSE",
}

UNKNOWN_LICENSE_VALUES = {
    "",
    "UNKNOWN",
    "NOASSERTION",
    "UNSPECIFIED",
    "N/A",
    "NONE",
    "NULL",
}

NON_USABLE_MARKERS = {
    "PROPRIETARY",
    "COMMERCIAL",
    "CUSTOM",
    "ALL RIGHTS RESERVED",
}


def _to_float(value: Any, default: float = 0.0) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def _clamp(value: float, low: float, high: float) -> float:
    return max(low, min(high, value))


def _normalize_0_10(value: Any) -> float:
    return _clamp(_to_float(value), 0.0, 10.0)


def maintenance_points(last_commit_days: float) -> float:
    if last_commit_days <= 30:
        return 25.0
    if last_commit_days <= 90:
        return 22.0
    if last_commit_days <= 180:
        return 18.0
    if last_commit_days <= 365:
        return 12.0
    if last_commit_days <= MAINTENANCE_GATE_DAYS:
        return 6.0
    return 0.0


def adoption_points(stars: float) -> float:
    if stars >= 20000:
        return 20.0
    if stars >= 5000:
        return 17.0
    if stars >= 1000:
        return 13.0
    if stars >= 300:
        return 9.0
    if stars >= 100:
        return 6.0
    return 3.0


def release_points(release_recency_days: float) -> float:
    if release_recency_days <= 30:
        return 15.0
    if release_recency_days <= 90:
        return 13.0
    if release_recency_days <= 180:
        return 10.0
    if release_recency_days <= 365:
        return 7.0
    if release_recency_days <= MAINTENANCE_GATE_DAYS:
        return 4.0
    return 2.0


def _parse_license_tokens(license_value: str) -> List[str]:
    normalized = license_value.upper().strip()
    if not normalized:
        return []
    normalized = normalized.replace("+", "-OR-LATER")
    tokens = re.split(r"\s+|\(|\)|/|,|;|\|", normalized)
    return [t for t in tokens if t and t not in {"AND", "OR", "WITH"}]


def _is_clear_license(license_value: str) -> bool:
    tokens = _parse_license_tokens(license_value)
    if not tokens:
        return False

    for token in tokens:
        if token in CLEAR_LICENSES:
            return True

    for token in tokens:
        if token.endswith("-ONLY") or token.endswith("-OR-LATER"):
            base = token.rsplit("-", 1)[0]
            if base in CLEAR_LICENSES:
                return True

    return False


def license_points_and_gate(license_value: Any) -> Tuple[float, bool, str]:
    text = str(license_value or "").strip()
    upper = text.upper()

    if upper in UNKNOWN_LICENSE_VALUES:
        return 0.0, False, "license is missing or unknown"

    if any(marker in upper for marker in NON_USABLE_MARKERS):
        return 0.0, False, "license is non-redistributable"

    if _is_clear_license(text):
        return 10.0, True, ""

    # Ambiguous but present text
    return 3.0, False, "license is ambiguous"


def compute_candidate(candidate: Dict[str, Any], pass_score: float, index: int) -> Dict[str, Any]:
    name = str(candidate.get("name") or f"candidate_{index}")
    archived = bool(candidate.get("archived", False))

    stars = _to_float(candidate.get("stars"), 0.0)
    last_commit_days = _to_float(candidate.get("last_commit_days"), 9999.0)
    release_recency = _to_float(candidate.get("release_recency"), 9999.0)

    docs_score = _normalize_0_10(candidate.get("docs_score"))
    quality_signals = _normalize_0_10(candidate.get("quality_signals"))
    fit_score = _normalize_0_10(candidate.get("fit_score"))

    maintenance = maintenance_points(last_commit_days)
    adoption = adoption_points(stars)
    docs = docs_score / 10.0 * 15.0
    release = release_points(release_recency)
    license_points, license_gate_ok, license_gate_reason = license_points_and_gate(
        candidate.get("license", "")
    )
    quality = quality_signals / 10.0 * 10.0
    fit = fit_score / 10.0 * 5.0

    gate_fail_reasons: List[str] = []
    if archived:
        gate_fail_reasons.append("repository is archived")
    if last_commit_days > MAINTENANCE_GATE_DAYS:
        gate_fail_reasons.append("no maintenance signal within 18 months")
    if not license_gate_ok:
        gate_fail_reasons.append(license_gate_reason)

    total = maintenance + adoption + docs + release + license_points + quality + fit
    total = round(total, 2)

    gates_passed = len(gate_fail_reasons) == 0
    if not gates_passed:
        verdict = "gate_failed"
    elif total >= pass_score:
        verdict = "mature_candidate"
    else:
        verdict = "candidate_with_risk"

    return {
        "name": name,
        "source_url": candidate.get("source_url", ""),
        "input": {
            "stars": stars,
            "last_commit_days": last_commit_days,
            "license": candidate.get("license", ""),
            "release_recency": release_recency,
            "docs_score": docs_score,
            "quality_signals": quality_signals,
            "fit_score": fit_score,
            "archived": archived,
        },
        "score_breakdown": {
            "maintenance_activity": round(maintenance, 2),
            "community_adoption": round(adoption, 2),
            "documentation_completeness": round(docs, 2),
            "release_stability": round(release, 2),
            "license_availability": round(license_points, 2),
            "engineering_quality_signals": round(quality, 2),
            "task_fit": round(fit, 2),
        },
        "total_score": total,
        "gates_passed": gates_passed,
        "gate_fail_reasons": gate_fail_reasons,
        "verdict": verdict,
    }


def build_top1_reason(ranked: List[Dict[str, Any]], pass_score: float) -> str:
    if not ranked:
        return "no candidates were provided"

    top = ranked[0]
    name = top.get("name", "unknown")
    score = top.get("total_score", 0)
    verdict = top.get("verdict", "unknown")

    if verdict == "mature_candidate":
        return (
            f"{name} ranked #1 with score {score} (>= {pass_score}), passed all hard gates, "
            "and is recommended as the reuse-first option."
        )

    if verdict == "candidate_with_risk":
        return (
            f"{name} ranked #1 with score {score} but did not reach mature threshold {pass_score}; "
            "recommend caution and gap mitigation before reuse."
        )

    reasons = top.get("gate_fail_reasons", [])
    reason_text = "; ".join(reasons) if reasons else "hard-gate failure"
    return (
        f"{name} ranked #1 by score {score} but failed hard gates ({reason_text}); "
        "no mature reusable candidate was found."
    )


def load_candidates(path: Path) -> List[Dict[str, Any]]:
    data = json.loads(path.read_text(encoding="utf-8"))

    if isinstance(data, list):
        candidates = data
    elif isinstance(data, dict) and isinstance(data.get("candidates"), list):
        candidates = data["candidates"]
    else:
        raise ValueError("input JSON must be a list or an object with key 'candidates'")

    normalized: List[Dict[str, Any]] = []
    for item in candidates:
        if isinstance(item, dict):
            normalized.append(item)
    return normalized


def main() -> int:
    parser = argparse.ArgumentParser(description="Score candidate reusable solutions deterministically")
    parser.add_argument("--input", required=True, help="Path to candidate JSON file")
    parser.add_argument("--output", help="Optional output JSON path")
    parser.add_argument("--min-pass-score", type=float, default=PASS_SCORE_DEFAULT)
    parser.add_argument("--pretty", action="store_true", help="Pretty-print JSON")
    args = parser.parse_args()

    input_path = Path(args.input).resolve()
    if not input_path.exists():
        print(f"[ERROR] Input file not found: {input_path}", file=sys.stderr)
        return 1

    try:
        candidates = load_candidates(input_path)
    except Exception as exc:  # noqa: BLE001
        print(f"[ERROR] Failed to load input JSON: {exc}", file=sys.stderr)
        return 1

    if not candidates:
        print("[ERROR] No valid candidates found in input", file=sys.stderr)
        return 1

    scored = [
        compute_candidate(candidate, args.min_pass_score, idx)
        for idx, candidate in enumerate(candidates, start=1)
    ]

    scored.sort(
        key=lambda c: (
            c["verdict"] != "gate_failed",  # keep gate failures lower priority
            c["total_score"],
            c["input"]["stars"],
            c["name"],
        ),
        reverse=True,
    )

    ranked = []
    for rank, item in enumerate(scored, start=1):
        cloned = dict(item)
        cloned["rank"] = rank
        ranked.append(cloned)

    result = {
        "pass_score": args.min_pass_score,
        "ranked": ranked,
        "top1_reason": build_top1_reason(ranked, args.min_pass_score),
    }

    indent = 2 if args.pretty else None
    output_text = json.dumps(result, ensure_ascii=False, indent=indent)

    if args.output:
        output_path = Path(args.output).resolve()
        output_path.write_text(output_text + "\n", encoding="utf-8")

    print(output_text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
