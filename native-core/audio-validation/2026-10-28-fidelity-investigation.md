# Audio fidelity investigation (2026-10-28)

The user reports widespread graininess, especially in bass, during ordinary
playback as well as transitions. Transparent mode and disabling Automix did
not resolve it. Older releases are not a confirmed clean baseline.

## Confirmed tempo-processing defect

WSOLA chose a shared stereo placement by correlating `(L + R) / 2`. Opposing
channels cancel that reference, so reversing only the right channel changed
the left output by up to 0.547 at 0.3 input amplitude, at rates 0.97 and 1.03.
The repair sums the two channel correlations and energies while retaining
one shared offset. A streaming polarity-invariance test fails before the
repair and passes afterwards. All 279 native tests pass (one existing ignored
test). Complete arm64 macOS and iOS Release builds also pass. This defect applies when tempo processing is active; it does not explain
all reported ordinary 1x playback distortion.

## Ordinary playback and source checks

- A six-second stereo PCM multitone, through the production decoder and
  Transparent mixer at 48 kHz, is sample-identical after its 272-frame protection
  delay and startup fade. No effects or protection interventions occurred.
- The reported track was Natalie Jane, "Somebody to Someone (I Just Wanna Fall
  in Love)", YouTube recording `sF2kOGBYFoU`. The selected source is stereo Opus,
  48 kHz, reported 146 kbps.
- A fresh download through the production resolver is byte-identical to the
  existing 2,695,128-byte cache entry. SHA-256:
  `45ce68eeddf5274279c5b100db205a2ca2f09ae09e45b9b249213d68bb5f1ad4`.
  All 7,885 audio packets have continuous timestamps, with no gaps/overlaps
  greater than 2 ms.
- Independently decoded the cached stream using PyAV/FFmpeg. A 30-second
  production decoder-stage capture agrees at approximately -85 dB relative RMS
  error. This is not evidence of a large decoding error.
- A 30-second physical AirPods output check of this recording in Transparent
  mode had zero callback underruns, zero device xruns, and no engine errors.
  It was muted; this verifies continuity, not listening quality.
- The user hears graininess in both an independent decode and a matched
  Transparent render, including vocals. Wired earphones also reproduce it.
  An independently generated low-level clean bass WAV sounds clean.
- The AirPods microphone was inactive when checked, so active headset-microphone
  degradation was not established as a cause. No system audio routes were changed.

## Commit-history and upstream findings

Refreshed `upstream/main` to `04a4a838`. Upstream's `AppSettings` starts
normalization enabled. `PlaybackService.loudnessGainMb` applies the source's
relative loudness with the correct sign, independently of EQ/spatial processing.

Our `188205fb` gated normalization behind Enhanced mode. `804f52e9` changed the
legacy default from enabled to disabled. Thus Transparent silently ignored even
an explicit Track/Album normalization selection. The source figure is already
handled correctly in `normalization_gain`; the policy gate prevented it running.

The reported recording carries +5.8100004 dB relative loudness. In the same
20-second production render starting at 100 seconds:

- Before: no loudness correction, 4,207 limiter interventions.
- After: -5.8100004 dB correction, zero limiter interventions, peak approximately
  0.5945, and approximately -82.3 dB relative error against independent decoding
  with the same constant source gain (no added nonlinear processing).

A production Load/render hot-bass regression test failed before the fix and
passes afterwards. Normalization now remains independent of sound mode through
loads, handoffs, swaps and telemetry. New tracks enter at their selected level
rather than driving the limiter during a startup gain ramp. The legacy/upstream
Track default is restored when no preference exists; explicit Off remains Off.
All 280 native tests pass (one existing ignored test), 952 shared tests pass,
and all 46 physical Mac engine checks pass, including Transparent On and Off.

The installed Mac initially advertised native revision `35bbf6bd7cc4`, but its
source fingerprint `4ddb92b12472` matches `4c0aa3e5`/v1.0.3, not that older
revision. This was stale build metadata, not stale DSP code. The fingerprint
comparison follows symlink contents, matching build.rs. The build now watches
the resolved branch ref as well as HEAD, including managed worktrees.

## Signed local installations

Complete Mac and signed iOS Release builds pass. Installed version 1.0.3 build 8
in place on Mac, iPhone and iPad, with post-install version readback on both
mobile devices. No uninstall or library/account/cache deletion was performed.
Mac retains the existing Developer ID identity, entitlements and valid profiles;
iPhone/iPad use Apple Development signing and device provisioning. Signatures
verify, Mac launch is confirmed, and the installed native source fingerprint
`6825701fcaa7` matches the tested source tree. Mobile runtime listening is not
claimed from installation readback.

## Remaining boundary

The normalization policy and stereo tempo regressions are reproducible and fixed.
This does not establish that every texture the user hears came from those faults.
The user could not reliably distinguish the independently supplied Apple/YouTube
excerpts. No further excerpt discrimination is required as a condition of work.

Our limiter still precedes the application's final volume gain; upstream's idle
float chain preserves headroom. That remaining ordering difference deserves a
separate design and real-time validation if distortion persists with normalization
explicitly Off or unavailable source metadata. Do not silently remove protection
or move expensive/allocating work into the device callback.

The installed builds are ready for normal listening confirmation. Preserve
libraries, accounts and existing music caches during any further investigation.
