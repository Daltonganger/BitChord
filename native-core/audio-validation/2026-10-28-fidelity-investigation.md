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

## Remaining boundary

Neither a general app regression nor a general fidelity fix has been established.
The reference excerpt used the same encoded source as BitChord, so hearing the
same problem there cannot by itself distinguish source/mastering artifacts from
an output issue. Matched excerpts from Apple's public AAC preview and the
YouTube Opus stream were prepared for a second source comparison. The user requested more matched regions before judging that comparison; three
eight-second pairs were prepared across approximately 1:00-1:30. Listening
confirmation of that comparison is still required.

Do not describe the tempo repair or PR #8 as fixing the general issue. Do not
replace all installed apps on that premise. Preserve libraries, accounts and
existing music caches; only temporary diagnostic copies were created.
