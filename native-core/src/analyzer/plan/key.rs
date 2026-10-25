//! Musical keys: parsing, distance, Camelot compatibility and clashes.

use super::analysis::Analysis;

pub(super) fn key_index(name: &str) -> Option<usize> {
    Some(match name {
        "C" => 0,
        "C♯" | "D♭" => 1,
        "D" => 2,
        "D♯" | "E♭" => 3,
        "E" => 4,
        "F" => 5,
        "F♯" | "G♭" => 6,
        "G" => 7,
        "G♯" | "A♭" => 8,
        "A" => 9,
        "A♯" | "B♭" => 10,
        "B" => 11,
        _ => return None,
    })
}

pub(super) fn split_key(key: &str) -> (Option<usize>, Option<&str>) {
    let mut parts = key.trim().split(' ');
    let index = parts.next().and_then(key_index);
    let mode = parts.next();
    (index, mode)
}

pub(super) fn key_distance(left: &str, right: &str) -> Option<usize> {
    let (left_index, left_mode) = split_key(left);
    let (right_index, right_mode) = split_key(right);
    let left_index = left_index?;
    let right_index = right_index?;
    let pitch = (left_index + 12 - right_index) % 12;
    let pitch = pitch.min((right_index + 12 - left_index) % 12);
    Some(
        pitch
            + if left_mode.is_some() && right_mode.is_some() && left_mode != right_mode {
                1
            } else {
                0
            },
    )
}

/// A key the analyzer was not confident about is no key at all.
pub(super) fn trusted_key(analysis: &Analysis) -> String {
    if analysis.key.is_empty() || analysis.key_confidence < 0.25 {
        String::new()
    } else {
        analysis.key.clone()
    }
}

/// Upstream `harmonicallyCompatible`: a second/fifth (same mode) or a second
/// (differing modes) is a move a DJ makes; anything wider is not.
pub(super) fn harmonically_compatible(left: &str, right: &str) -> bool {
    let (left_index, left_mode) = split_key(left);
    let (right_index, right_mode) = split_key(right);
    let left_index = match left_index {
        Some(i) => i,
        None => return false,
    };
    let right_index = match right_index {
        Some(i) => i,
        None => return false,
    };
    let distance = (left_index + 12 - right_index) % 12;
    let distance = distance.min((right_index + 12 - left_index) % 12);
    if left_mode.is_some() && right_mode.is_some() && left_mode != right_mode {
        return distance <= 1;
    }
    distance <= 2 || distance == 5
}

/// Do two confident keys clash?
///
/// Both sides have to be trusted — a key the analyzer is unsure of is no key at
/// all. Relative major and minor (the same number on the Camelot wheel) and its
/// neighbours never clash, and neither do keys the semitone distance already
/// calls harmonically close; a clash needs to fail both tests.
pub(super) fn keys_clash(
    left_key: &str,
    left_confidence: f64,
    right_key: &str,
    right_confidence: f64,
) -> bool {
    if left_confidence < 0.25 || right_confidence < 0.25 {
        return false;
    }
    if left_key.is_empty() || right_key.is_empty() {
        return false;
    }
    if camelot_compatible(left_key, right_key) {
        return false;
    }
    !harmonically_compatible(left_key, right_key)
}

/// Camelot-wheel neighbours: the same number across the letters (relative major
/// and minor), or one step either way with the same letter.
pub(super) fn camelot_compatible(left: &str, right: &str) -> bool {
    let Some((left_number, left_minor)) = camelot(left) else {
        return false;
    };
    let Some((right_number, right_minor)) = camelot(right) else {
        return false;
    };
    if left_number == right_number {
        return left_minor != right_minor;
    }
    if left_minor != right_minor {
        return false;
    }
    let step = (left_number + 12 - right_number) % 12;
    step == 1 || step == 11
}

/// A key's Camelot number (1…12) and whether it is a minor key.
pub(super) fn camelot(key: &str) -> Option<(usize, bool)> {
    let (index, mode) = split_key(key);
    let index = index?;
    let minor = mode? == "minor";
    const MINOR: [usize; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
    const MAJOR: [usize; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
    Some(if minor {
        (MINOR[index], true)
    } else {
        (MAJOR[index], false)
    })
}
