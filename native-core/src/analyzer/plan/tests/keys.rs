//! Key compatibility, Camelot neighbours and key distance.

use super::*;

#[test]
fn camelot_neighbours_and_relative_keys_never_clash() {
    // C major and A minor: the same number on the wheel.
    assert!(!keys_clash("C major", 0.6, "A minor", 0.6));
    // A minor (8A) and E minor (9A): one step, same letter.
    assert!(!keys_clash("A minor", 0.6, "E minor", 0.6));
    // C major (8B) and G major (9B): one step the other way, and a fifth.
    assert!(!keys_clash("C major", 0.6, "G major", 0.6));
    // C major against F♯ major: six semitones and no wheel relationship.
    assert!(keys_clash("C major", 0.6, "F♯ major", 0.6));
    // One side under the trust gate is no key at all.
    assert!(!keys_clash("C major", 0.24, "F♯ major", 0.9));
    assert!(!keys_clash("C major", 0.9, "F♯ major", 0.10));
    // The Camelot numbers themselves.
    assert_eq!(camelot("A minor"), Some((8, true)));
    assert_eq!(camelot("C major"), Some((8, false)));
    assert_eq!(camelot("E minor"), Some((9, true)));
    assert_eq!(camelot("G major"), Some((9, false)));
    assert_eq!(camelot(""), None);
}

#[test]
fn phrase_switch_refuses_incompatible_keys() {
    let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    out.key = "C major".into();
    out.key_confidence = 0.9;
    out.bpm = 120.0;
    out.beat_confidence = 0.8;
    let mut inc = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    inc.key = "D♯ major".into(); // a minor third — not a compatible move
    inc.key_confidence = 0.9;
    inc.bpm = 120.0;
    inc.beat_confidence = 0.8;
    assert!(phrase_switch(&out, &inc).is_none());
}

#[test]
fn harmonically_compatible_matches_upstream_semantics() {
    // A fifth and a second are compatible; a minor third is not.
    assert!(harmonically_compatible("C major", "G major"));
    assert!(harmonically_compatible("C major", "D major"));
    assert!(!harmonically_compatible("C major", "D♯ major"));
    // Differing modes are compatible only within a second.
    assert!(harmonically_compatible("C major", "C minor"));
    assert!(!harmonically_compatible("C major", "D minor"));
    assert!(!harmonically_compatible("C major", "G minor"));
    // Unknown keys are never compatible.
    assert!(!harmonically_compatible("", "C major"));
}

#[test]
fn key_distance_matches_upstream_semantics() {
    assert_eq!(key_distance("C major", "C major"), Some(0));
    // A fifth is the closest relationship on the circle.
    assert_eq!(key_distance("C major", "G major"), Some(5));
    // Same pitch class but different mode charges one step.
    assert_eq!(key_distance("C major", "C minor"), Some(1));
    // An unknown key yields no distance.
    assert_eq!(key_distance("", "C major"), None);
    // A low-confidence key is not trusted.
    let mut a = analysis(0.0, 0.0, vec![]);
    a.key = "C major".into();
    a.key_confidence = 0.1;
    assert_eq!(trusted_key(&a), "");
}
