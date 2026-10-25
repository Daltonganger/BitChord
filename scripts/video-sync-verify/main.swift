import Foundation

// The picture-follows-the-engine contract, with fake clock values.
//
// The O9 picture player is a muted AVPlayer slaved to the audio engine: seek
// past 150 ms of drift, nudge the rate by at most ±0.1 inside that, pause and
// resume with the transport. None of that needs a device or a stream to check —
// it is arithmetic on two positions — so this exercises the real policy with
// the boundary cases that are easy to get wrong (exactly at the threshold, the
// cooldown, the ignore band, a paused scrub, and a speed-changed engine).
//
// Run: scripts/check-video-sync.sh

var failures = 0
var checks = 0

func pass(_ label: String, _ detail: String) {
    checks += 1
    print("  ok   \(label)\(detail.isEmpty ? "" : " — \(detail)")")
}

func fail(_ label: String, _ detail: String) {
    checks += 1
    failures += 1
    print("  FAIL \(label)\(detail.isEmpty ? "" : " — \(detail)")")
}

func checkAction(
    _ label: String,
    _ got: VideoSyncPolicy.Action,
    _ want: VideoSyncPolicy.Action
) {
    if got == want {
        pass(label, "")
    } else {
        fail(label, "got \(got), want \(want)")
    }
}

func checkRate(
    _ label: String,
    _ got: VideoSyncPolicy.Action,
    _ want: Float
) {
    if case .play(let rate) = got, abs(rate - want) < 0.0005 {
        pass(label, String(format: "%.3fx", rate))
    } else {
        fail(label, "got \(got), want play(\(want))")
    }
}

let policy = VideoSyncPolicy.self

// 1. In sync: the picture just runs.
checkAction("in sync does nothing",
            policy.decide(engineSeconds: 100, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
            .none)

// 2. Past the seek threshold: jump the picture to the engine.
checkAction("half a second behind seeks to the engine",
            policy.decide(engineSeconds: 100.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 0, secondsSinceSeek: 1),
            .seek(seconds: 100.5))
checkAction("half a second ahead seeks back to the engine",
            policy.decide(engineSeconds: 99.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
            .seek(seconds: 99.5))

// 3. Inside the seek threshold: nudge speed, never snap. 0.1 s × 0.8 = 1.08x.
checkRate("0.1 s behind speeds the picture up",
          policy.decide(engineSeconds: 100.1, videoSeconds: 100, enginePlaying: true,
                        engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
          1.08)
checkRate("0.1 s ahead slows the picture down",
          policy.decide(engineSeconds: 99.9, videoSeconds: 100, enginePlaying: true,
                        engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
          0.92)

// 4. The nudge is clamped at ±0.1 even when the drift is at the seek edge.
checkRate("the rate nudge is clamped at 1.1x",
          policy.decide(engineSeconds: 100.14, videoSeconds: 100, enginePlaying: true,
                        engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
          1.1)

// 5. The ignore band: 20 ms is not worth a rate change, but a stopped picture
//    still has to start.
checkAction("20 ms of drift is ignored while the picture runs",
            policy.decide(engineSeconds: 100.02, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
            .none)
checkRate("a stopped picture starts even inside the ignore band",
          policy.decide(engineSeconds: 100.02, videoSeconds: 100, enginePlaying: true,
                        engineRate: 1, videoRate: 0, secondsSinceSeek: 1),
          1.0)

// 6. Already at the target rate: no command storm.
checkAction("already at the nudged rate does nothing",
            policy.decide(engineSeconds: 100.1, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1.08, secondsSinceSeek: 1),
            .none)

// 7. Paused transport: stop the picture first, then move a scrubbed one.
checkAction("a paused transport pauses a moving picture",
            policy.decide(engineSeconds: 100, videoSeconds: 99, enginePlaying: false,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
            .pause)
checkAction("a paused scrub seeks the still picture",
            policy.decide(engineSeconds: 100, videoSeconds: 99, enginePlaying: false,
                          engineRate: 1, videoRate: 0, secondsSinceSeek: 1),
            .seek(seconds: 100))

// 8. One seek per cooldown: a slow seek must not become a seek loop.
checkAction("the seek cooldown blocks a second seek",
            policy.decide(engineSeconds: 100.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 0.1),
            .play(rate: 1.1))
checkAction("the cooldown expires at 0.5 s",
            policy.decide(engineSeconds: 100.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 0.5),
            .seek(seconds: 100.5))

// 8b. A seek on a streamed picture lands late; aim where the engine will be,
//     or the picture seeks forever and shows one frame per seek (owner's Mac).
checkAction("a playing engine is sought ahead by the measured seek time",
            policy.decide(engineSeconds: 100.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 0, secondsSinceSeek: 1, seekLead: 0.4),
            .seek(seconds: 100.9))
checkAction("the lead scales with a faster engine",
            policy.decide(engineSeconds: 100.5, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1.5, videoRate: 0, secondsSinceSeek: 1, seekLead: 0.4),
            .seek(seconds: 101.1))
checkAction("a paused engine is sought exactly, with no lead",
            policy.decide(engineSeconds: 100, videoSeconds: 99, enginePlaying: false,
                          engineRate: 1, videoRate: 0, secondsSinceSeek: 1, seekLead: 0.4),
            .seek(seconds: 100))
checkRate("a picture that landed on the lead plays rather than seeking again",
          policy.decide(engineSeconds: 100.89, videoSeconds: 100.9, enginePlaying: true,
                        engineRate: 1, videoRate: 0, secondsSinceSeek: 0.4, seekLead: 0.4),
          1.0)

// 9. Speed-changed engine: the picture matches the engine's rate.
checkRate("a 1.5x engine runs the picture at 1.5x",
          policy.decide(engineSeconds: 100, videoSeconds: 100, enginePlaying: true,
                        engineRate: 1.5, videoRate: 0, secondsSinceSeek: 1),
          1.5)
checkRate("the picture cannot be pushed past the player's 2x ceiling",
          policy.decide(engineSeconds: 100.1, videoSeconds: 100, enginePlaying: true,
                        engineRate: 2, videoRate: 0, secondsSinceSeek: 1),
          2.0)

// 10. Nonsense positions (an item still loading, a missing duration) are not a
//     reason to command anything.
checkAction("a non-finite position is ignored",
            policy.decide(engineSeconds: .nan, videoSeconds: 100, enginePlaying: true,
                          engineRate: 1, videoRate: 1, secondsSinceSeek: 1),
            .none)

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
