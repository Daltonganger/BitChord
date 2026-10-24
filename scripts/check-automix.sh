#!/bin/bash
# AutoMix A′: the analysed blend is the default, and effects are only earned by
# a real clash. The planner cases that encode that rule are Rust tests; the
# corpus scorecard needs the owner's real pairs and is skipped until
# AUTOMIX_CORPUS points at one (>= 12 usable pairs).
#
# Run: scripts/check-automix.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$repo/native-core" || exit 2

cargo test --lib analyzer::plan::tests::effect_earned_only_for_a_real_clash || exit 1
cargo test --lib analyzer::plan::tests::a_five_percent_gap_keeps_the_analysed_blend || exit 1
cargo test --lib analyzer::plan::tests::a_true_beatmatch_still_blends || exit 1
cargo test --lib analyzer::plan::tests::mismatched_tempos_get_a_dj_technique_not_a_fade || exit 1

if [ -n "${AUTOMIX_CORPUS:-}" ]; then
  echo "automix: corpus at $AUTOMIX_CORPUS — scorecard harness is A′3 (needs beat/vocal models)"
else
  echo "automix: plan-order checks passed; no AUTOMIX_CORPUS (need ≥ 12 real pairs)"
fi
