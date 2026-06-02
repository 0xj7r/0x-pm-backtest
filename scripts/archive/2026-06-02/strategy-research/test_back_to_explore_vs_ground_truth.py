#!/usr/bin/env python3
"""
Proper behavioral testing for BackToExplore against real wallet ground truth.

Usage:
  python scripts/test_back_to_explore_vs_ground_truth.py \
    --summary data/runs/back_to_explore_2.7k_proper/summary.json \
    --ground-truth 79.0 7.24 51 15-17

Compares key metrics to the 33-day profile the user provided.
"""
import json
import argparse

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--summary", required=True)
    parser.add_argument("--profile", default=None, help="Optional profile.json from lively_run_profile.py for two-sided/clip stats")
    parser.add_argument("--ground-truth-two-sided", type=float, default=79.0, help="Real two-sided rate from 33-day profile")
    parser.add_argument("--ground-truth-median-clip", type=float, default=7.24, help="Real median clip USD")
    parser.add_argument("--ground-truth-median-entry-pct", type=float, default=51.0, help="Real median percent through window")
    parser.add_argument("--ground-truth-peak-hours", default="15-17", help="Real peak UTC hours")
    args = parser.parse_args()

    with open(args.summary) as f:
        data = json.load(f)

    bt = data["per_strategy"]["back_to_explore"]

    print("=== BackToExplore vs Real Ground Truth (33-day profile) ===")
    print(f"Markets processed: {data['markets_attempted']}")

    two_sided = "N/A"
    median_clip = "N/A"
    if args.profile:
        with open(args.profile) as pf:
            prof = json.load(pf)
        two_sided = f"{prof.get('two_sided_fraction', 0)*100:.1f}"
        median_clip = f"${prof.get('sizing', {}).get('median_usd', 0):.2f}"
    print(f"\nTwo-sided rate:     {two_sided}%   (real: {args.ground_truth_two_sided}%)")
    print(f"Median clip:        {median_clip}   (real: ${args.ground_truth_median_clip})")

    print(f"Max DD:             {bt['path_max_drawdown_pct']:.2f}%   (aim <15% for world-class on 2.7k)")
    print(f"Compounded return:  {bt['compounded_return_pct']:.2f}%")
    print(f"Hit rate:           {bt['hit_rate']*100:.1f}%")

    print("\nRun this after the backtest finishes for full behavioral comparison:")
    print("  python scripts/lively_run_profile.py --markets-jsonl ... --strategy back_to_explore --out profile.json")
    print("  python scripts/test_back_to_explore_vs_ground_truth.py --summary summary.json --profile profile.json")

if __name__ == "__main__":
    main()
