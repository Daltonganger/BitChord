import Foundation

// The stored enum values are a wire contract; production uses PlaybackController's enum.
enum PlaybackController { enum RepeatMode: Int { case off, all, one } }
@main struct VerifySessionRestore {
    static func main() {
        let domain = "app.bitchord.tests.session.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: domain)!
        defer { defaults.removePersistentDomain(forName: domain) }
        let key = "bitchord_last_played"
        func track(_ index: Int, explicit: Bool? = nil) -> QueueEntry {
            QueueEntry(id: "track-\(index)", title: "Song \(index)", artist: "Artist", source: "yt:track-\(index)", isLocal: false, contextOrder: index, isExplicit: explicit)
        }
        func save(_ tracks: [QueueEntry], index: Int = 0, volume: Double = 0, position: Double = 137.25) {
            LastPlayed.save(tracks: tracks, index: index, position: position, repeatMode: .all, shuffleEnabled: true, volume: volume, defaults: defaults)
        }
        save([track(0, explicit: false)])
        var restored = LastPlayed.load(defaults: defaults)!
        precondition(restored.volume == 0, "Muted playback must remain muted after restart")
        precondition(restored.position == 137.25 && restored.repeatMode == .all && restored.shuffleEnabled)
        precondition(restored.tracks[0].isExplicit == false)
        save([track(1, explicit: true)], volume: 0.4)
        precondition(LastPlayed.load(defaults: defaults)!.tracks[0].isExplicit == true)
        defaults.set(Data("broken json".utf8), forKey: key)
        restored = LastPlayed.load(defaults: defaults)!
        precondition(restored.tracks[0].id == "track-0" && restored.volume == 0, "Corruption must recover the previous valid snapshot")
        save([track(2)], volume: .nan, position: .infinity)
        restored = LastPlayed.load(defaults: defaults)!
        precondition(restored.position == 0 && restored.volume == 0.9)
        save([track(3)], volume: 4, position: -10)
        precondition(LastPlayed.load(defaults: defaults)!.volume == 1 && LastPlayed.load(defaults: defaults)!.position == 0)
        let entries = (0..<150).map { track($0) }
        LastPlayed.save(tracks: entries, index: 100, position: 37, repeatMode: .off, shuffleEnabled: false, volume: 0,
            contextEntries: entries, orderedContextEntries: entries, contextCursor: 150, contextBaseIndex: 0, defaults: defaults)
        restored = LastPlayed.load(defaults: defaults)!
        precondition(restored.tracks.count == 60 && restored.tracks[restored.index].id == "track-100")
        precondition(restored.contextBaseIndex == 90 && restored.contextCursor == 150, "Trimming the persisted queue must preserve its canonical context offset")
        var legacy = try! JSONSerialization.jsonObject(with: defaults.data(forKey: key)!) as! [String: Any]
        legacy["tracks"] = [["id":"legacy", "title":"Legacy", "artist":"Artist", "source":"yt:legacy", "isLocal":false, "fromAutoplay":false]]
        legacy["index"] = 0
        defaults.set(try! JSONSerialization.data(withJSONObject: legacy), forKey: key)
        precondition(LastPlayed.load(defaults: defaults)!.tracks[0].isExplicit == nil)
        print("PASS session mute, position, repeat/shuffle, edition, previous-snapshot recovery, invalid numbers, bounded queue/context and legacy snapshots")
    }
}
