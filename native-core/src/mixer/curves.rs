//! Gain curves and filter rides for every transition style: the equal-power
//! pair, beds and dips, cold ends, effect intros, and the swept filters.

use super::fx;
use super::types::{TransitionPlan, TransitionStyle};
use crate::transition_filter::TransitionFilter;

pub(super) const FILTER_ENTRY_HZ: f64 = 7_000.0;
pub(super) const FILTER_FLOOR_HZ: f64 = 300.0;
pub(super) const BASS_SWAP_HZ: f64 = 200.0;
pub(super) const BASS_SWAP_WIDTH: f64 = 0.10;
pub(super) const FILTER_SWEEP_SHAPE: f64 = 0.75;
pub(super) const ENTRY_HIGH_PASS_HZ: f64 = 1_200.0;
pub(super) const ENTRY_OPEN_BY: f64 = 0.6;
pub(super) const ENTRY_SHAPE: f64 = 0.35;
pub(super) const VOCAL_SEPARATION_FLOOR_HZ: f64 = 1_600.0;
pub(super) const VOCAL_SEPARATION_HIGH_PASS_HZ: f64 = 700.0;
pub(super) const BLEND_ENTRY_HIGH_PASS_HZ: f64 = 520.0;
pub(super) const BLEND_ENTRY_OPEN_BY: f64 = 0.45;
pub(super) const BLEND_ENTRY_CLASH_HIGH_PASS_HZ: f64 = 950.0;
pub(super) const BLEND_ENTRY_CLASH_OPEN_BY: f64 = 0.7;
pub(super) const BLEND_EXIT_FROM: f64 = 0.3;
pub(super) const BLEND_EXIT_LOW_PASS_HZ: f64 = 2_200.0;
pub(super) const BLEND_EXIT_CLASH_FROM: f64 = 0.12;
pub(super) const BLEND_EXIT_CLASH_LOW_PASS_HZ: f64 = 1_100.0;

/// Deepest dip a plan may ask for, as a fraction of the incoming gain.
///
/// A one-beat notch of this depth is about −9 dB on the record being brought
/// in. The outgoing track is untouched, so the blend dips a few dB and
/// recovers. Deeper than this reads as the transition losing the thread.
pub(crate) const MAX_DUCK_DEPTH: f64 = 0.65;

/// How long the incoming bed takes to rise out of silence, so the blend does
/// not open on a step.
pub(super) const BED_ATTACK_SECONDS: f64 = 0.040;

/// Equal-power fall: `cos` of the half turn. Used by the bail ramp and as half
/// of the plain crossfade.
pub(super) fn fall_gain(progress: f64) -> f32 {
    (progress.clamp(0.0, 1.0) * core::f64::consts::PI / 2.0).cos() as f32
}

pub(super) fn rise_gain(progress: f64) -> f32 {
    (progress.clamp(0.0, 1.0) * core::f64::consts::PI / 2.0).sin() as f32
}

/// Smoothstep: zero slope at both ends.
pub(super) fn smoothstep(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

pub(super) fn db_to_gain(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// One-beat notch in the incoming gain, centred just before the bass swap.
///
/// Incoming only. The record already playing is the reference; dipping it
/// would dip the song the listener came for. Because the notch sits in a gain
/// that is otherwise climbing, the shape is "gains, quickly loses, gains".
pub(super) fn dip_gain(progress: f64, plan: &TransitionPlan) -> f32 {
    let depth = plan.dip_depth.clamp(0.0, MAX_DUCK_DEPTH);
    if depth <= 0.0 {
        return 1.0;
    }
    let width = plan.dip_width.clamp(0.015, 0.25);
    let swap = plan.bass_swap_fraction.clamp(0.05, 0.95);
    let centre = (swap - width * 0.5).clamp(0.02, 0.95);
    let x = (progress.clamp(0.0, 1.0) - centre) / (width * 0.5);
    (1.0 - depth * (-x * x).exp()) as f32
}

/// DJ envelope: a ramp across the whole blend, with a bed at the open.
///
/// * The incoming track eases up from silence over ~40 ms, then climbs to
///   unity across the rest of the blend. A one-beat dip can still notch that
///   climb just before the low end changes hands.
/// * The outgoing track stays at full level while that bed is opening, then
///   follows a cos fall over the same span the incoming is climbing. The two
///   move together. Holding the outgoing at unity until a late swap, then
///   dropping it, is a cut once the blend itself is short.
pub(super) fn automix_gains(progress: f64, plan: &TransitionPlan, hold: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let swap = plan.bass_swap_fraction.clamp(0.15, 0.92);
    // A cold ending holds the outgoing until its final chord even through the
    // DJ bed's own fall point: the chord has to be struck at unity.
    let bed_end = plan
        .bed_fraction
        .clamp(0.05, swap - 0.02)
        .max(hold)
        .clamp(0.05, 0.95);
    let bed = db_to_gain(plan.bed_gain_db).clamp(0.02, 0.5);
    let attack = if plan.fade_seconds > 0.0 {
        (BED_ATTACK_SECONDS / plan.fade_seconds).clamp(0.004, 0.12)
    } else {
        0.02
    };
    let rise = if p <= attack {
        bed * smoothstep(p / attack)
    } else {
        let t = ((p - attack) / (1.0 - attack)).clamp(0.0, 1.0);
        bed + (1.0 - bed) * smoothstep(t)
    };
    let fall = if p <= bed_end {
        1.0
    } else {
        let t = ((p - bed_end) / (1.0 - bed_end)).clamp(0.0, 1.0);
        (t * core::f64::consts::PI / 2.0).cos()
    };
    ((rise * dip_gain(p, plan) as f64) as f32, fall as f32)
}

/// The incoming's level under the held outgoing during a cold ending's phrase,
/// in dB **relative to its own matched level**: the same scale into which
/// `mix_into` folds the transition match and the incoming's normalization. It
/// is never an absolute level, so a quiet record and a loud one both sit 12 dB
/// under the chord. The next record has to be audible under the final chord,
/// but an equal-power rise across the whole blend would have it at 0.7 by the
/// time the chord is struck — a +2 dB lift exactly where the listener is paying
/// attention to the ending. Rising to an underlay first keeps that below a
/// decibel and still lets the incoming take over across the decay.
pub(super) const COLD_END_UNDERLAY_DB: f64 = -12.0;

/// The `(rise, fall)` pair when a cold ending holds the outgoing at unity until
/// its final chord, at `hold` (a fraction of the fade).
///
/// Before the chord the outgoing is untouched and the incoming lifts gently to
/// the underlay; from the chord the two cross over an equal-power fall that
/// ends exactly with the record, so the chord's attack is at unity and its decay
/// carries the incoming up. The summed power stays within about 0.5 dB of unity
/// across the hold and the crossing.
pub(super) fn cold_end_gains(progress: f64, hold: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let h = hold.clamp(0.0, 0.95);
    if h <= 0.0 {
        return fade_gains(p, false);
    }
    let underlay = db_to_gain(COLD_END_UNDERLAY_DB);
    if p <= h {
        // The chord is still ahead: the outgoing is at unity and the next
        // record is only establishing itself underneath it.
        let rise = underlay * smoothstep(p / h);
        return (rise as f32, 1.0);
    }
    let angle = ((p - h) / (1.0 - h)) * core::f64::consts::FRAC_PI_2;
    let rise = underlay + (1.0 - underlay) * angle.sin();
    (rise as f32, angle.cos() as f32)
}

/// The incoming's level by the throw when an effect plan does not carry one.
pub(super) const EFFECT_INTRO_DB: f64 = -4.0;
/// Under the outgoing the incoming's bass is held out: one low end at a time,
/// and the next record's arrives on the landing.
pub(super) const EFFECT_INTRO_HIGH_PASS_HZ: f64 = 300.0;
/// With `filter_sweep` at 1 (two grids that do not agree) the incoming's top
/// is held this low too, so its hats never tick against the outgoing's.
pub(super) const EFFECT_INTRO_LOW_PASS_HZ: f64 = 3_500.0;

/// The `(rise, fall)` pair on an effect handover with an intro of `hold`
/// (a fraction of the fade).
///
/// Across the intro the outgoing stays at unity and the incoming climbs to
/// `intro_db`, filtered by [`ride_effect_intro`]: plainly audible, the two
/// records playing together. From the throw the technique shapes the outgoing
/// (its own dry curve, so `fall` matters only until then) and the incoming
/// rises the rest of the way.
pub(super) fn effect_intro_gains(progress: f64, hold: f64, intro_db: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let h = hold.clamp(0.0, 0.95);
    if h <= 0.0 {
        return fade_gains(p, false);
    }
    let intro = db_to_gain(intro_db.min(0.0));
    if p <= h {
        return ((intro * smoothstep(p / h)) as f32, 1.0);
    }
    let angle = ((p - h) / (1.0 - h)) * core::f64::consts::FRAC_PI_2;
    let rise = intro + (1.0 - intro) * angle.sin();
    (rise as f32, angle.cos() as f32)
}

/// The incoming's filter on an effect handover: bass out until the landing,
/// and on a grid clash (`band` 1) the top held back until the throw, opening
/// across it, so the record is whole when it lands.
pub(super) fn ride_effect_intro(
    progress: f64,
    hold: f64,
    landing: f64,
    band: f64,
    inc: &mut TransitionFilter,
) {
    if progress >= landing {
        inc.open();
        return;
    }
    let opening = if landing > hold {
        ((progress - hold) / (landing - hold)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let amount = band.clamp(0.0, 1.0) * (1.0 - opening);
    let low_pass = glide(
        TransitionFilter::open_hz() as f64,
        EFFECT_INTRO_LOW_PASS_HZ,
        amount,
    );
    inc.set_cutoffs(low_pass as f32, EFFECT_INTRO_HIGH_PASS_HZ as f32);
}

/// Where an effect plan's incoming lands, as a fraction of a fade of
/// `fade_seconds` whose last `1 - hold` is the technique window.
pub(super) fn effect_landing(plan: &TransitionPlan, hold: f64) -> f64 {
    let land = fx::landing_fraction_for_window(
        plan.style,
        plan.beat_seconds,
        plan.effect_seconds,
    )
    .unwrap_or(0.5);
    hold + (1.0 - hold) * land
}

pub(super) fn effect_intro_db(plan: &TransitionPlan) -> f64 {
    if plan.bed_gain_db < 0.0 {
        plan.bed_gain_db
    } else {
        EFFECT_INTRO_DB
    }
}

/// A plan's hold as a fraction of the fade actually being played. The engine's
/// span can be shorter than the plan's `fade_seconds` (a short incoming is
/// capped at a third of itself), and the hold is anchored to the blend start,
/// not to the plan.
pub(super) fn hold_fraction(plan: &TransitionPlan, fade_seconds: f64) -> f64 {
    if fade_seconds <= 0.0 {
        0.0
    } else {
        (plan.outgoing_hold_seconds / fade_seconds).clamp(0.0, 0.95)
    }
}

/// The `(rise, fall)` gain pair for fade progress `p`.
///
/// Which pair is correct depends on whether the two sides carry the same
/// signal, and getting that wrong is audible either way:
///
/// * Different tracks with no DJ envelope are uncorrelated, so their powers
///   add and the equal-power pair holds the level still (`rise² + fall² = 1`).
/// * A source swap is one recording on both sides, aligned, so the amplitudes
///   add. The equal-power pair would then peak at `√2·sin(p·π/2 + π/4)` —
///   +3.01 dB through the middle of every upgrade. A linear pair sums to 1.
/// * A planned DJ blend is neither: [`automix_gains`] holds the outgoing track
///   up while the incoming one plays underneath, then hands over.
pub(super) fn fade_gains(progress: f64, same_signal: bool) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    if same_signal {
        (p as f32, (1.0 - p) as f32)
    } else {
        (rise_gain(p), fall_gain(p))
    }
}

/// Geometric interpolation between two cutoffs — pitch is logarithmic.
pub(super) fn glide(from: f64, to: f64, amount: f64) -> f64 {
    from * (to / from).powf(amount.clamp(0.0, 1.0))
}

pub(super) fn entry_high_pass(progress: f64, amount: f64, top_hz: f64, open_by: f64) -> f32 {
    let remaining = (1.0 - progress / open_by).clamp(0.0, 1.0);
    glide(
        TransitionFilter::off_hz() as f64,
        top_hz,
        amount * remaining.powf(ENTRY_SHAPE),
    ) as f32
}

pub(super) fn bass_cutoff(amount: f64) -> f32 {
    glide(TransitionFilter::off_hz() as f64, BASS_SWAP_HZ, amount) as f32
}

pub(super) fn blend_exit_low_pass(progress: f64, clash: f64) -> f32 {
    let from = BLEND_EXIT_FROM + (BLEND_EXIT_CLASH_FROM - BLEND_EXIT_FROM) * clash;
    let amount = ((progress - from) / (1.0 - from)).clamp(0.0, 1.0);
    let floor = glide(BLEND_EXIT_LOW_PASS_HZ, BLEND_EXIT_CLASH_LOW_PASS_HZ, clash);
    glide(TransitionFilter::open_hz() as f64, floor, amount) as f32
}

pub(super) fn ride_filter_sweep(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let sweep = plan.filter_sweep.clamp(0.0, 1.0);
    if sweep <= 0.0 {
        out.open();
        inc.open();
        return;
    }
    let open = TransitionFilter::open_hz() as f64;
    let entry = glide(open, FILTER_ENTRY_HZ, sweep);
    let floor = glide(open, FILTER_FLOOR_HZ, sweep);
    let cutoff = glide(entry, floor, progress.powf(FILTER_SWEEP_SHAPE));
    out.set_cutoffs(cutoff as f32, TransitionFilter::off_hz());
    inc.set_cutoffs(
        TransitionFilter::open_hz(),
        entry_high_pass(progress, sweep, ENTRY_HIGH_PASS_HZ, ENTRY_OPEN_BY),
    );
}

pub(super) fn ride_vocal_separation(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let amount = plan.vocal_overlap.clamp(0.0, 1.0);
    if amount <= 0.0 {
        out.open();
        inc.open();
        return;
    }
    let open = TransitionFilter::open_hz() as f64;
    let floor = glide(open, VOCAL_SEPARATION_FLOOR_HZ, amount);
    out.set_cutoffs(
        glide(open, floor, progress.powf(FILTER_SWEEP_SHAPE)) as f32,
        TransitionFilter::off_hz(),
    );
    inc.set_cutoffs(
        TransitionFilter::open_hz(),
        entry_high_pass(
            progress,
            amount,
            VOCAL_SEPARATION_HIGH_PASS_HZ,
            ENTRY_OPEN_BY,
        ),
    );
}

pub(super) fn ride_bass_swap(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let swap_at = plan.bass_swap_fraction.clamp(0.05, 0.95);
    let handover = ((progress - swap_at) / BASS_SWAP_WIDTH * 0.5 + 0.5).clamp(0.0, 1.0);
    let clash = plan.vocal_overlap.clamp(0.0, 1.0);
    // The entry high-pass has to be open by the swap. Left on its own clock
    // it outlasted an early swap: the outgoing had already handed its low end
    // over while the incoming was still filtered at ~300 Hz, and for seconds
    // neither record had a kick or a bass line — only hats and the click of
    // the beat, which is heard as hiss.
    let open_by = (BLEND_ENTRY_OPEN_BY
        + (BLEND_ENTRY_CLASH_OPEN_BY - BLEND_ENTRY_OPEN_BY) * clash)
        .min(swap_at);
    let entry = bass_cutoff(1.0 - handover).max(entry_high_pass(
        progress,
        1.0,
        glide(
            BLEND_ENTRY_HIGH_PASS_HZ,
            BLEND_ENTRY_CLASH_HIGH_PASS_HZ,
            clash,
        ),
        open_by,
    ));
    inc.set_cutoffs(TransitionFilter::open_hz(), entry);
    out.set_cutoffs(blend_exit_low_pass(progress, clash), bass_cutoff(handover));
}

pub(super) fn ride_filters(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    match plan.style {
        TransitionStyle::DjFilter => ride_filter_sweep(progress, plan, out, inc),
        TransitionStyle::DjBlend => {
            if plan.bass_swap {
                ride_bass_swap(progress, plan, out, inc)
            } else {
                ride_vocal_separation(progress, plan, out, inc)
            }
        }
        // An album played through: any filtering would be an edit the record
        // didn't ask for.
        TransitionStyle::Gapless => {
            out.open();
            inc.open();
        }
        TransitionStyle::EqualPower => ride_vocal_separation(progress, plan, out, inc),
        // The effect techniques do their own filtering on the outgoing deck.
        TransitionStyle::EchoOut
        | TransitionStyle::LoopRoll
        | TransitionStyle::ReverbWash
        | TransitionStyle::DropCut
        | TransitionStyle::Texture
        | TransitionStyle::Brake => {
            out.open();
            inc.open();
        }
    }
}

// ---- Voices ----------------------------------------------------------------
