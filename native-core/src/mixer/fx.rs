//! Deck effects for the DJ techniques that do not need two matching tempos.
//!
//! A crossfade, however it is filtered, needs the two records to sit on top of
//! each other for a while, which only sounds right when their grids agree. The
//! techniques here are the ones a DJ reaches for when they don't: the outgoing
//! record is *finished off* by an effect — an echo, a beat repeat, a reverb, a
//! filter build — and the incoming one lands on its cue.
//!
//! They still cross over. Every technique ramps the outgoing dry down while the
//! incoming ramps up over an overlap of **two beats or one second, whichever is
//! longer** (a quarter beat, minimum 60 ms, for DropCut), so the incoming is
//! never a step from silence and the outgoing dry never cuts while the incoming
//! is still silent. Echo and reverb tails ring over the crossing. Loop roll and
//! cut grow their windows by the overlap; the echo and the wash already have
//! room after their landing.
//!
//! Each technique is a schedule over the transition window, in frames:
//!
//! | technique   | outgoing deck                                   | incoming lands |
//! |-------------|--------------------------------------------------|----------------|
//! | Echo out    | echo thrown on the last beat; dry ramps down over the overlap; tail rings out | after 1 beat |
//! | Loop roll   | captures a beat, repeats ½, ¼, ⅛ under a rising high-pass; then ramps out over the overlap | at the old window end |
//! | Reverb wash | dry dissolves while the send rises; incoming rises over the overlap; tail washes over | halfway |
//! | Cut         | a high-pass build, then a quarter-beat (min 60 ms) crossfade | at the cut |
//!
//! The incoming voice is *held* (not decoded) until its landing frame, so its
//! cue is the downbeat it lands on, exactly — not a guess at where it will be
//! after the window has run.

use super::TransitionStyle;

/// The shortest overlap an effect handover may use (except DropCut).
const MIN_OVERLAP_SECONDS: f64 = 1.0;
/// The overlap in beats, so a slower record gets a longer crossing.
const OVERLAP_BEATS: f64 = 2.0;
/// DropCut stays a cut: a quarter-beat edge, never shorter than this.
const DROP_CUT_MIN_SECONDS: f64 = 0.06;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Technique {
    EchoOut,
    LoopRoll,
    ReverbWash,
    Cut,
}

impl Technique {
    pub(crate) fn of(style: TransitionStyle) -> Option<Self> {
        match style {
            TransitionStyle::EchoOut => Some(Self::EchoOut),
            TransitionStyle::LoopRoll => Some(Self::LoopRoll),
            TransitionStyle::ReverbWash => Some(Self::ReverbWash),
            TransitionStyle::DropCut => Some(Self::Cut),
            _ => None,
        }
    }

    /// The base window, in beats, and where in it the incoming record lands.
    /// Loop roll and cut add the overlap on top of this; the others already
    /// have that much room after their landing.
    pub(crate) fn shape(self) -> (f64, f64) {
        match self {
            // One beat to throw the echo, four for it to ring out under the
            // new record.
            Self::EchoOut => (5.0, 1.0 / 5.0),
            // Capture one beat, then ½, ¼ and ⅛ repeats, one beat each.
            Self::LoopRoll => (4.0, 1.0),
            // Four beats of wash, the new record arriving halfway.
            Self::ReverbWash => (8.0, 0.5),
            // A two-beat build into the cut.
            Self::Cut => (2.0, 1.0),
        }
    }

    /// Whether the window has to grow by the overlap after the landing.
    fn extends_window(self) -> bool {
        matches!(self, Self::LoopRoll | Self::Cut)
    }
}

/// The crossing length for a technique, in seconds.
fn overlap_seconds(style: TransitionStyle, beat_seconds: f64) -> f64 {
    let Some(technique) = Technique::of(style) else {
        return 0.0;
    };
    let beat = sane_beat(beat_seconds);
    match technique {
        Technique::Cut => (beat / 4.0).max(DROP_CUT_MIN_SECONDS),
        _ => (beat * OVERLAP_BEATS).max(MIN_OVERLAP_SECONDS),
    }
}

/// The window length a technique needs at `beat_seconds`.
pub(crate) fn window_seconds(style: TransitionStyle, beat_seconds: f64) -> Option<f64> {
    let technique = Technique::of(style)?;
    let beat = sane_beat(beat_seconds);
    let base = technique.shape().0 * beat;
    Some(if technique.extends_window() {
        base + overlap_seconds(style, beat_seconds)
    } else {
        base
    })
}

/// Where the incoming lands, as a fraction of [`window_seconds`].
pub(crate) fn landing_fraction(style: TransitionStyle, beat_seconds: f64) -> Option<f64> {
    let technique = Technique::of(style)?;
    let beat = sane_beat(beat_seconds);
    let base = technique.shape().0 * beat;
    let land = technique.shape().1 * base;
    let window = window_seconds(style, beat_seconds)?;
    Some((land / window).clamp(0.0, 1.0))
}

fn sane_beat(beat_seconds: f64) -> f64 {
    if beat_seconds.is_finite() && (0.25..=1.5).contains(&beat_seconds) {
        beat_seconds
    } else {
        0.5
    }
}

pub(crate) struct DeckFx {
    technique: Technique,
    rate: f64,
    beat_frames: usize,
    window_frames: u64,
    land_frame: u64,
    overlap_frames: u64,
    elapsed: u64,
    delay: StereoDelay,
    reverb: Reverb,
    roll: BeatRepeat,
    build: HighPass,
}

impl DeckFx {
    pub(crate) fn new(style: TransitionStyle, device_rate: u32, beat_seconds: f64) -> Option<Self> {
        let technique = Technique::of(style)?;
        let rate = device_rate.max(8_000) as f64;
        let beat = sane_beat(beat_seconds);
        let beat_frames = (beat * rate).round().max(1.0) as usize;
        let (base_beats, base_land) = technique.shape();
        let base_window_frames = (base_beats * beat_frames as f64).round().max(1.0) as u64;
        let land_frame = (base_land * base_window_frames as f64).round() as u64;
        let wanted_overlap = (overlap_seconds(style, beat_seconds) * rate).round().max(1.0) as u64;
        let (window_frames, overlap_frames) = if technique.extends_window() {
            (base_window_frames + wanted_overlap, wanted_overlap)
        } else {
            (
                base_window_frames,
                wanted_overlap.min(base_window_frames.saturating_sub(land_frame)).max(1),
            )
        };
        // A dotted eighth: the delay DJs set by default, because it lands
        // between the beats instead of doubling them.
        let echo_frames = (beat_frames as f64 * 0.75).round() as usize;
        Some(Self {
            technique,
            rate,
            beat_frames,
            window_frames: window_frames.max(1),
            land_frame,
            overlap_frames,
            elapsed: 0,
            delay: StereoDelay::new(echo_frames.max(1), rate),
            reverb: Reverb::new(rate),
            roll: BeatRepeat::new(beat_frames),
            build: HighPass::default(),
        })
    }

    /// Whether the incoming voice should be decoding yet. It is held until
    /// the first chunk at or after its landing frame, so its first rendered
    /// sample is its cue — never a chunk decoded at zero gain and lost.
    #[allow(dead_code)]
    pub(crate) fn incoming_live(&self) -> bool {
        self.elapsed >= self.land_frame
    }

    /// Where the technique's own landing falls, in window frames (for the
    /// render harness).
    pub(crate) fn land_frame(&self) -> u64 {
        self.land_frame
    }

    /// Level of the incoming voice for the next sample: nothing before it
    /// lands, then a sine up over the overlap.
    #[allow(dead_code)]
    pub(crate) fn incoming_gain(&self) -> f32 {
        if !self.incoming_live() {
            return 0.0;
        }
        let into = (self.elapsed - self.land_frame) as f64;
        let p = (into / self.overlap_frames.max(1) as f64).clamp(0.0, 1.0);
        (p * core::f64::consts::FRAC_PI_2).sin() as f32
    }

    /// The outgoing dry level for the next sample. Echo, loop roll and cut are
    /// 1 at the landing and cosine down over the overlap; the wash keeps its
    /// own dissolve into the landing (the reverb tail covers the crossing).
    pub(crate) fn dry_gain(&self) -> f32 {
        match self.technique {
            Technique::ReverbWash => {
                let p = (self.elapsed as f64 / self.land_frame.max(1) as f64).clamp(0.0, 1.0);
                (p * core::f64::consts::FRAC_PI_2).cos() as f32
            }
            _ => {
                if self.elapsed < self.land_frame {
                    return 1.0;
                }
                let p = ((self.elapsed - self.land_frame) as f64
                    / self.overlap_frames.max(1) as f64)
                    .clamp(0.0, 1.0);
                (p * core::f64::consts::FRAC_PI_2).cos() as f32
            }
        }
    }

    #[allow(dead_code)]
    #[allow(dead_code)]
    pub(crate) fn finished(&self) -> bool {
        self.elapsed >= self.window_frames
    }

    /// Renders `frames` stereo frames of the outgoing deck through the
    /// technique and adds them to `dest`. `input` may be short — the outgoing
    /// file can end inside the window — and the effect tails keep sounding.
    pub(crate) fn render(&mut self, input: &[f32], dest: &mut [f32], frames: usize) {
        let window = self.window_frames as f64;
        let land = self.land_frame as f64;
        let beat = self.beat_frames as f64;
        for i in 0..frames.min(dest.len() / 2) {
            let n = self.elapsed as f64;
            let (l, r) = if i * 2 + 1 < input.len() {
                (input[i * 2], input[i * 2 + 1])
            } else {
                (0.0, 0.0)
            };
            let (out_l, out_r) = match self.technique {
                Technique::EchoOut => {
                    let dry = self.dry_gain();
                    // Thrown on the beat before the landing; nothing after it
                    // feeds the echo, so it rings out rather than piling up.
                    let send = if n < land {
                        (((n - (land - beat)) / (beat * 0.25)).clamp(0.0, 1.0)) as f32
                    } else {
                        0.0
                    };
                    let (wl, wr) = self.delay.process(l * send, r * send);
                    // The last beat of the window fades what is left.
                    let tail = ((window - n) / beat).clamp(0.0, 1.0) as f32;
                    (l * dry + wl * tail, r * dry + wr * tail)
                }
                Technique::LoopRoll => {
                    let segment = (n / beat).floor() as usize;
                    let (sl, sr) = if segment == 0 {
                        self.roll.capture(l, r);
                        (l, r)
                    } else {
                        // ½, ¼, ⅛ of the captured beat.
                        let divisor = 1usize << segment.min(3);
                        self.roll.repeat(self.beat_frames / divisor)
                    };
                    // The rising high-pass across the roll is the build; it
                    // reaches its corner at the landing and holds through the
                    // crossing while the dry ramps out.
                    let rise = if land > beat {
                        ((n - beat) / (land - beat)).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    self.build.set(glide(20.0, 900.0, rise * rise), self.rate);
                    let (hl, hr) = self.build.process(sl, sr);
                    let out = self.dry_gain();
                    (hl * out, hr * out)
                }
                Technique::ReverbWash => {
                    let dry = self.dry_gain();
                    let p = (n / land).clamp(0.0, 1.0);
                    let send = (p.sqrt()) as f32;
                    let (wl, wr) = self.reverb.process(l * send, r * send);
                    let tail = ((window - n) / (window - land).max(1.0)).clamp(0.0, 1.0) as f32;
                    (l * dry + wl * tail, r * dry + wr * tail)
                }
                Technique::Cut => {
                    let rise = (n / land).clamp(0.0, 1.0);
                    self.build.set(glide(20.0, 600.0, rise * rise * rise), self.rate);
                    let (hl, hr) = self.build.process(l, r);
                    let out = self.dry_gain();
                    (hl * out, hr * out)
                }
            };
            dest[i * 2] += out_l;
            dest[i * 2 + 1] += out_r;
            self.elapsed += 1;
        }
    }
}

fn glide(from: f64, to: f64, amount: f64) -> f64 {
    from * (to / from).powf(amount.clamp(0.0, 1.0))
}

/// Feedback delay with the repeats darkened and thinned on every pass, the way
/// a tape or dub echo loses top and bottom — so the tail sits behind the new
/// record instead of fighting its kick and its vocal.
struct StereoDelay {
    buffer: Vec<f32>,
    write: usize,
    length: usize,
    feedback: f32,
    tone: [OnePole; 2],
    thin: [HighPass1; 2],
}

impl StereoDelay {
    fn new(length: usize, rate: f64) -> Self {
        let mut tone = [OnePole::default(), OnePole::default()];
        let mut thin = [HighPass1::default(), HighPass1::default()];
        for t in &mut tone {
            t.set(2_800.0, rate);
        }
        for t in &mut thin {
            t.set(220.0, rate);
        }
        Self {
            buffer: vec![0.0; length * 2],
            write: 0,
            length,
            feedback: 0.52,
            tone,
            thin,
        }
    }

    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let i = self.write * 2;
        let (dl, dr) = (self.buffer[i], self.buffer[i + 1]);
        let fl = self.thin[0].process(self.tone[0].process(dl));
        let fr = self.thin[1].process(self.tone[1].process(dr));
        // Ping-pong: each repeat crosses sides.
        self.buffer[i] = l + fr * self.feedback;
        self.buffer[i + 1] = r + fl * self.feedback;
        self.write = (self.write + 1) % self.length;
        (dl, dr)
    }
}

/// One beat captured, then replayed in shrinking slices.
struct BeatRepeat {
    buffer: Vec<f32>,
    captured: usize,
    cursor: usize,
    slice: usize,
}

impl BeatRepeat {
    fn new(beat_frames: usize) -> Self {
        Self {
            buffer: vec![0.0; beat_frames * 2],
            captured: 0,
            cursor: 0,
            slice: beat_frames,
        }
    }

    fn capture(&mut self, l: f32, r: f32) {
        if self.captured * 2 + 1 < self.buffer.len() {
            self.buffer[self.captured * 2] = l;
            self.buffer[self.captured * 2 + 1] = r;
            self.captured += 1;
        }
    }

    fn repeat(&mut self, slice: usize) -> (f32, f32) {
        let slice = slice.clamp(1, self.captured.max(1));
        if slice != self.slice {
            self.slice = slice;
            self.cursor = 0;
        }
        if self.captured == 0 {
            return (0.0, 0.0);
        }
        let i = self.cursor % slice;
        self.cursor = (self.cursor + 1) % slice;
        // A few frames of fade at each slice edge, so the loop seam is a
        // stutter and not a click.
        let edge = 64.min(slice / 4).max(1) as f32;
        let from_start = i as f32;
        let to_end = (slice - 1 - i) as f32;
        let g = (from_start / edge).min(to_end / edge).min(1.0);
        (self.buffer[i * 2] * g, self.buffer[i * 2 + 1] * g)
    }
}

/// Schroeder–Moorer reverb (Freeverb's tunings, scaled to the device rate):
/// eight damped combs into four all-passes per side, a long, dark room.
struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<AllPass>; 2],
}

const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

impl Reverb {
    fn new(rate: f64) -> Self {
        let scale = rate / 44_100.0;
        let side = |spread: usize| {
            (
                COMB_TUNING
                    .iter()
                    .map(|t| Comb::new(((t + spread) as f64 * scale) as usize, 0.88, 0.3))
                    .collect::<Vec<_>>(),
                ALLPASS_TUNING
                    .iter()
                    .map(|t| AllPass::new(((t + spread) as f64 * scale) as usize))
                    .collect::<Vec<_>>(),
            )
        };
        let (cl, al) = side(0);
        let (cr, ar) = side(STEREO_SPREAD);
        Self {
            combs: [cl, cr],
            allpasses: [al, ar],
        }
    }

    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let input = (l + r) * 0.015;
        let mut out = [0.0f32; 2];
        for side in 0..2 {
            let mut acc = 0.0;
            for comb in &mut self.combs[side] {
                acc += comb.process(input);
            }
            for allpass in &mut self.allpasses[side] {
                acc = allpass.process(acc);
            }
            out[side] = acc;
        }
        (out[0], out[1])
    }
}

struct Comb {
    buffer: Vec<f32>,
    index: usize,
    feedback: f32,
    damp: f32,
    store: f32,
}

impl Comb {
    fn new(length: usize, feedback: f32, damp: f32) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            index: 0,
            feedback,
            damp,
            store: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.buffer[self.index];
        self.store = output * (1.0 - self.damp) + self.store * self.damp;
        self.buffer[self.index] = input + self.store * self.feedback;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

struct AllPass {
    buffer: Vec<f32>,
    index: usize,
}

impl AllPass {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let buffered = self.buffer[self.index];
        let output = buffered - input;
        self.buffer[self.index] = input + buffered * 0.5;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

#[derive(Default)]
struct OnePole {
    a: f32,
    z: f32,
}

impl OnePole {
    fn set(&mut self, hz: f64, rate: f64) {
        self.a = (1.0 - (-2.0 * core::f64::consts::PI * hz / rate).exp()) as f32;
    }

    fn process(&mut self, x: f32) -> f32 {
        self.z += self.a * (x - self.z);
        self.z
    }
}

#[derive(Default)]
struct HighPass1 {
    low: OnePole,
}

impl HighPass1 {
    fn set(&mut self, hz: f64, rate: f64) {
        self.low.set(hz, rate);
    }

    fn process(&mut self, x: f32) -> f32 {
        x - self.low.process(x)
    }
}

/// Stereo 12 dB/oct high-pass (two one-pole stages): the build.
#[derive(Default)]
struct HighPass {
    stages: [[HighPass1; 2]; 2],
}

impl HighPass {
    fn set(&mut self, hz: f64, rate: f64) {
        for side in &mut self.stages {
            for stage in side {
                stage.set(hz, rate);
            }
        }
    }

    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let [left, right] = &mut self.stages;
        let l = left[0].process(l);
        let l = left[1].process(l);
        let r = right[0].process(r);
        let r = right[1].process(r);
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, rate: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (i as f64 * 440.0 * 2.0 * core::f64::consts::PI / rate).sin() as f32 * 0.5;
                [s, s]
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Render one frame at a time until `target`, so gains can be sampled
    /// exactly at the landing and across the crossing.
    fn advance_to(fx: &mut DeckFx, target: u64) {
        let input = tone(2, 48_000.0);
        let mut out = vec![0.0f32; 2];
        while fx.elapsed < target && !fx.finished() {
            fx.render(&input, &mut out, 1);
        }
    }

    #[test]
    fn every_technique_finishes_its_window_without_clipping() {
        for style in [
            TransitionStyle::EchoOut,
            TransitionStyle::LoopRoll,
            TransitionStyle::ReverbWash,
            TransitionStyle::DropCut,
        ] {
            let rate = 48_000u32;
            let mut fx = DeckFx::new(style, rate, 0.5).unwrap();
            let input = tone(512, rate as f64);
            let mut rendered = 0u64;
            let mut loudest = 0.0f32;
            while !fx.finished() {
                let mut out = vec![0.0f32; 1024];
                fx.render(&input, &mut out, 512);
                loudest = loudest.max(peak(&out));
                rendered += 512;
                assert!(rendered < 10 * rate as u64, "{style:?} never finished");
            }
            assert!(loudest < 1.0, "{style:?} peaked at {loudest}");
            assert!(fx.incoming_gain() > 0.99, "{style:?} left the incoming down");
        }
    }

    #[test]
    fn the_incoming_is_held_then_ramps_over_the_overlap() {
        let mut fx = DeckFx::new(TransitionStyle::EchoOut, 48_000, 0.5).unwrap();
        assert!(!fx.incoming_live());
        assert_eq!(fx.incoming_gain(), 0.0);
        let land = fx.land_frame;
        let overlap = fx.overlap_frames;
        advance_to(&mut fx, land);
        assert_eq!(fx.elapsed, land);
        assert_eq!(fx.incoming_gain(), 0.0);
        assert_eq!(fx.dry_gain(), 1.0);
        advance_to(&mut fx, land + overlap);
        assert_eq!(fx.elapsed, land + overlap);
        assert!((fx.incoming_gain() - 1.0).abs() < 0.001);
        assert!(fx.dry_gain() < 0.001);
    }

    #[test]
    fn echo_loop_and_cut_cross_over_equal_power() {
        for style in [
            TransitionStyle::EchoOut,
            TransitionStyle::LoopRoll,
            TransitionStyle::DropCut,
        ] {
            let rate = 48_000u32;
            let mut fx = DeckFx::new(style, rate, 0.5).unwrap();
            let land = fx.land_frame;
            let overlap = fx.overlap_frames;
            advance_to(&mut fx, land);
            assert!((fx.dry_gain() - 1.0).abs() < 0.001, "{style:?} dry at landing");
            assert!(fx.incoming_gain() < 0.001, "{style:?} incoming at landing");
            advance_to(&mut fx, land + overlap / 2);
            let dry = fx.dry_gain();
            let incoming = fx.incoming_gain();
            let power = dry * dry + incoming * incoming;
            assert!(
                (power - 1.0).abs() < 0.01,
                "{style:?} crossing power {power} (dry {dry}, incoming {incoming})"
            );
            advance_to(&mut fx, land + overlap);
            assert!(fx.dry_gain() < 0.001, "{style:?} dry at crossing end");
            assert!((fx.incoming_gain() - 1.0).abs() < 0.001, "{style:?} incoming at crossing end");
        }
    }

    #[test]
    fn reverb_wash_keeps_its_own_dry_dissolve_and_a_ramped_incoming() {
        let mut fx = DeckFx::new(TransitionStyle::ReverbWash, 48_000, 0.5).unwrap();
        let land = fx.land_frame;
        let overlap = fx.overlap_frames;
        advance_to(&mut fx, land / 2);
        let half = fx.dry_gain();
        assert!(half > 0.6 && half < 0.8, "wash dry at half {half}");
        advance_to(&mut fx, land);
        assert!(fx.dry_gain() < 0.001, "wash dry at landing");
        assert!(fx.incoming_gain() < 0.001, "wash incoming at landing");
        advance_to(&mut fx, land + overlap / 2);
        assert!((fx.incoming_gain() - core::f64::consts::FRAC_1_SQRT_2 as f32).abs() < 0.01);
        advance_to(&mut fx, land + overlap);
        assert!((fx.incoming_gain() - 1.0).abs() < 0.001);
    }

    #[test]
    fn drop_cut_keeps_a_short_overlap() {
        let rate = 48_000f64;
        let fx = DeckFx::new(TransitionStyle::DropCut, rate as u32, 0.5).unwrap();
        let seconds = fx.overlap_frames as f64 / rate;
        assert!(
            (0.06..=0.13).contains(&seconds),
            "DropCut overlap {seconds}s"
        );
    }

    #[test]
    fn only_the_window_extending_techniques_grow() {
        let beat = 0.5;
        assert!((window_seconds(TransitionStyle::EchoOut, beat).unwrap() - 2.5).abs() < 1e-9);
        assert!((window_seconds(TransitionStyle::ReverbWash, beat).unwrap() - 4.0).abs() < 1e-9);
        assert!((window_seconds(TransitionStyle::LoopRoll, beat).unwrap() - 3.0).abs() < 1e-9);
        assert!((window_seconds(TransitionStyle::DropCut, beat).unwrap() - 1.125).abs() < 1e-9);
        assert!((landing_fraction(TransitionStyle::LoopRoll, beat).unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert!((landing_fraction(TransitionStyle::EchoOut, beat).unwrap() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn the_echo_rings_on_after_the_dry_signal_stops() {
        let mut fx = DeckFx::new(TransitionStyle::EchoOut, 48_000, 0.5).unwrap();
        let input = tone(512, 48_000.0);
        while !fx.incoming_live() {
            let mut out = vec![0.0; 1024];
            fx.render(&input, &mut out, 512);
        }
        // Past the landing, with the outgoing file ended: only the tail.
        let mut tail = 0.0f32;
        for _ in 0..40 {
            let mut out = vec![0.0; 1024];
            fx.render(&[], &mut out, 512);
            tail = tail.max(peak(&out));
        }
        assert!(tail > 0.05, "echo tail was {tail}");
    }
}
