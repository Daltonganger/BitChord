import Foundation

// The Handoff payload, from the app's own source.
//
// What this can prove is that the two devices agree about the bytes. A payload
// one build writes and the other cannot read is a handoff that silently does
// nothing, and no build tells you that: the sender believes it published and the
// receiver believes there was nothing to receive.

var failures = 0
var checks = 0

func check(_ label: String, _ ok: Bool, _ detail: String = "") {
    checks += 1
    if ok {
        print("  ok   \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    } else {
        failures += 1
        print("  FAIL \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    }
}

print("handoff: what one device hands the other")

let payload = PlaybackHandoffPayload(
    videoId: "dQw4w9WgXcQ",
    title: "A Song",
    artist: "An Artist",
    positionMs: 90_500,
    isPlaying: true
)
let info = payload.userInfo

// ---- the write side -------------------------------------------------------

// One key, and a value `userInfo` can actually carry: it accepts property-list
// types only, and a bare struct would be dropped without an error.
check("the payload rides in a single userInfo key", info.count == 1 && info["payload"] is String)
check(
    "the userInfo is a valid property list",
    PropertyListSerialization.propertyList(info, isValidFor: .binary)
)

// ---- and back -------------------------------------------------------------

check("it round-trips through userInfo", PlaybackHandoffPayload(userInfo: info) == payload)
check("the title and artist survive", PlaybackHandoffPayload(userInfo: info)?.title == "A Song"
      && PlaybackHandoffPayload(userInfo: info)?.artist == "An Artist")
check("the position converts from milliseconds", payload.positionSeconds == 90.5)

// A paused track must arrive paused: the flag is part of the payload rather than
// something the receiver infers from a missing position.
let paused = PlaybackHandoffPayload(videoId: "abc123", title: "T", artist: "A", positionMs: 0, isPlaying: false)
check("a paused payload keeps its flag", PlaybackHandoffPayload(userInfo: paused.userInfo) == paused)

// A negative position is clamped where the payload is made, so the receiver
// never has to decide what a seek to -3 s means.
let rewound = PlaybackHandoffPayload(videoId: "abc123", title: "T", artist: "A", positionMs: -4_000, isPlaying: true)
check("a negative position is clamped", rewound.positionMs == 0 && rewound.positionSeconds == 0)

// ---- what must NOT be taken -----------------------------------------------

// Nothing at all: a different activity type reuses an empty userInfo.
check("an absent userInfo is not a payload", PlaybackHandoffPayload(userInfo: nil) == nil)
check("an empty userInfo is not a payload", PlaybackHandoffPayload(userInfo: [:]) == nil)

// Somebody else's activity.
check(
    "another activity's userInfo is not a payload",
    PlaybackHandoffPayload(userInfo: ["foo": "bar"]) == nil
)

// Truncated or corrupted JSON must be refused, not half-read.
check(
    "a corrupted payload is refused",
    PlaybackHandoffPayload(userInfo: ["payload": "not json"]) == nil
)
check(
    "a truncated payload is refused",
    PlaybackHandoffPayload(userInfo: ["payload": #"{"videoId":"abc""#]) == nil
)

// An older or newer sender: this build writes one version and understands one.
let json = (info["payload"] as? String) ?? ""
let future = json.replacingOccurrences(of: "\"version\":1", with: "\"version\":2")
check("a payload from a newer schema is ignored", PlaybackHandoffPayload(userInfo: ["payload": future]) == nil,
      "version 2")

// A payload with no track to play is useless even when its shape is right — an
// empty id would start a queue item the engine cannot resolve.
let noTrack = #"{"version":1,"videoId":"","title":"T","artist":"A","positionMs":0,"isPlaying":false}"#
check("a payload with no track id is refused", PlaybackHandoffPayload(userInfo: ["payload": noTrack]) == nil)

// A field missing means the sender and receiver disagree about the schema, which
// is the same answer as a version this build does not know.
let missingField = #"{"version":1,"title":"T","artist":"A","positionMs":0,"isPlaying":false}"#
check("a payload missing its track id is refused",
      PlaybackHandoffPayload(userInfo: ["payload": missingField]) == nil)

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
