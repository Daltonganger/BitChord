import Foundation

let tracks: [(number: UInt32, path: String)] = [
    (0, "/music/10-unmatched.m4a"),
    (3, "/music/03-three.m4a"),
    (1, "/music/99-one.m4a"),
    (0, "/music/2-unmatched.m4a"),
    (2, "/music/02-two.m4a")
]
let ordered = tracks.sorted {
    localTrackComesBefore(
        trackNumber: $0.number, path: $0.path,
        trackNumber: $1.number, path: $1.path
    )
}
let expected = ["/music/99-one.m4a", "/music/02-two.m4a", "/music/03-three.m4a",
                "/music/2-unmatched.m4a", "/music/10-unmatched.m4a"]
guard ordered.map(\.path) == expected else {
    fputs("FAIL album ordering: \(ordered.map(\.path))\n", stderr)
    exit(1)
}
print("PASS local album track-number order and stable natural path fallback")
