import Foundation

@main struct VerifyStreamRecovery {
    static func main() {
        precondition(PlaybackRecoveryPolicy.canRecover(entrySource: "https://library.test/song", loadedSource: "https://library.test/song", origin: "local"))
        precondition(!PlaybackRecoveryPolicy.canRecover(entrySource: "/local/song.flac", loadedSource: "/local/song.flac", origin: "local"))
        precondition(!PlaybackRecoveryPolicy.canRecover(entrySource: "yt:track", loadedSource: "/downloads/saved.flac", origin: "local"))
        precondition(PlaybackRecoveryPolicy.canRecover(entrySource: "yt:track", loadedSource: "/cache/growing", origin: "cache"))
        var policy = PlaybackRecoveryPolicy()
        policy.select("one", resetAttempts: true)
        precondition(!policy.observe(position: 10, now: 0, playing: true, bufferedFrames: 0))
        precondition(!policy.observe(position: 10, now: 11.9, playing: true, bufferedFrames: 0))
        precondition(policy.observe(position: 10, now: 12, playing: true, bufferedFrames: 0))
        precondition(!policy.observe(position: 10, now: 13, playing: false, bufferedFrames: 0))
        precondition(!policy.observe(position: 10, now: 100, playing: true, bufferedFrames: 0))
        precondition(!policy.observe(position: 10, now: 120, playing: true, bufferedFrames: 2048))
        precondition(!policy.observe(position: 2, now: 121, playing: true, bufferedFrames: 0)) // Seek.
        precondition(!policy.observe(position: 2.5, now: 122, playing: true, bufferedFrames: 0)) // Progress.
        precondition(policy.claimRetry(position: 2.5, duration: 200))
        policy.select("one", resetAttempts: false)
        precondition(policy.claimRetry(position: 2.5, duration: 200))
        policy.select("one", resetAttempts: false)
        precondition(!policy.claimRetry(position: 2.5, duration: 200))
        policy.select("two", resetAttempts: false)
        precondition(!policy.claimRetry(position: 198, duration: 200)) // Natural end.
        precondition(policy.claimRetry(position: 2.5, duration: 200))
        policy.select("two", resetAttempts: true) // Explicit replay owns a new budget.
        precondition(policy.claimRetry(position: 2.5, duration: 200))
        print("PASS stream stall, pause, buffered output, seek, progress, retry budget, new track and natural end")
    }
}
