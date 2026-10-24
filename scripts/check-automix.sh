#!/bin/bash
# AutoMix: the planner's rules, and the scorecard over the owner's real pairs.
#
# The rule cases are Rust tests. The scorecard reads the app's own stream cache
# (read-only, titles only) and plans twelve pairs through the same entry the app
# calls, so it measures the planner on real audio rather than on fixtures.
#
# The corpus half runs in --release: the beat and vocal models are ONNX graphs,
# and in a debug profile one three-minute track takes minutes instead of a
# second, which makes the check unusable rather than thorough.
#
# Run: scripts/check-automix.sh            (corpus at the default cache path)
#      AUTOMIX_CORPUS=/path scripts/check-automix.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$repo/native-core" || exit 2

cargo test --lib analyzer::plan::tests::effect_earned_only_for_a_real_clash || exit 1
cargo test --lib analyzer::plan::tests::a_five_percent_gap_keeps_the_analysed_blend || exit 1
cargo test --lib analyzer::plan::tests::a_true_beatmatch_still_blends || exit 1
cargo test --lib analyzer::plan::tests::mismatched_tempos_get_a_dj_technique_not_a_fade || exit 1
cargo test --lib analyzer::plan::tests::a_spent_effect_budget_keeps_the_blend || exit 1

corpus="${AUTOMIX_CORPUS:-$HOME/Library/Caches/BitChord/audio}"
if [ ! -d "$corpus" ]; then
  echo "automix: plan-order checks passed; no corpus at $corpus"
  exit 0
fi

out="$(AUTOMIX_CORPUS="$corpus" RUST_LOG=native_core=info \
  cargo test --release --lib automix_corpus_scorecard -- --ignored --nocapture 2>&1)" || {
  echo "$out" | tail -20
  exit 1
}

echo "$out" | grep -E '^ *[0-9]+\. |^automix scorecard'
summary="$(echo "$out" | grep '^automix scorecard: usable' | tail -1)"
if [ -z "$summary" ]; then
  echo "automix: the scorecard printed no summary"
  exit 1
fi
median="$(echo "$summary" | sed -E 's/.*median fade ([0-9.]+)s.*/\1/')"
effects="$(echo "$summary" | sed -E 's/.*effects ([0-9]+)\/([0-9]+).*/\1/')"
pairs="$(echo "$summary" | sed -E 's/.*effects ([0-9]+)\/([0-9]+).*/\2/')"
minfade="$(echo "$summary" | sed -E 's/.*min fade ([0-9.]+)s.*/\1/')"

fail=0
# Enforced: freshness. A session may not be mostly effects.
if [ "$pairs" -gt 0 ]; then
  if [ $((effects * 100)) -le $((pairs * 25)) ]; then
    echo "ok   effects are within the 25% budget — $effects of $pairs"
  else
    echo "FAIL effects are $effects of $pairs, over the 25% budget"
    fail=1
  fi
fi

# Reported: length. These are the owner's bar and are met by the transition
# length work (E), not by this rule set, so they are printed, not enforced yet.
awk -v m="$median" 'BEGIN { if (m + 0 >= 8.0) print "ok   median overlap " m "s is at least a phrase pair (>= 8s)"; else print "target median overlap " m "s is under 8s" }'
awk -v m="$minfade" 'BEGIN { if (m + 0 >= 4.0) print "ok   shortest overlap " m "s is at least 4s (DropCut exempt)"; else print "target shortest overlap " m "s is under 4s" }'

exit "$fail"
