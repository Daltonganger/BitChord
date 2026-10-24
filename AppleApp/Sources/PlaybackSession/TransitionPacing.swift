import Foundation

/// The two tracks a transition joins: the outgoing track's id and the next one's.
struct TransitionPair: Hashable {
    let outgoing: String
    let incoming: String
}

/// A′3's session pacing, kept pure so its two easy-to-get-wrong rules have a
/// check: a transition is keyed by the pair it joins, and a pair never counts
/// against its own budget.
///
/// The pair key matters because the growing-file loop and the quality-upgrade
/// path re-plan the *same* transition several times. Keyed by style alone, one
/// transition filled the window with copies of itself, and the re-plan saw the
/// effect its own first attempt had chosen, flipped to a blend, and the effect
/// could never survive an attempt.
struct TransitionPacing<Style> {
    struct Entry {
        let pair: TransitionPair
        let style: Style
    }

    /// Oldest first, at most four.
    private(set) var entries: [Entry] = []

    /// No effect among the last three *other* transitions, so the fourth at
    /// most may use one. The pair being planned is ignored: its entry is the
    /// attempt being replaced, not a transition behind it.
    func allowsEffect(for pair: TransitionPair, isEffect: (Style) -> Bool) -> Bool {
        let others = entries.filter { $0.pair != pair }
        return !others.suffix(3).contains(where: { isEffect($0.style) })
    }

    /// A plan that was actually queued. A re-plan of the same pair replaces
    /// that pair's entry, and moves it to the newest position, rather than
    /// filling the window with copies of one transition.
    mutating func note(_ style: Style, for pair: TransitionPair) {
        entries.removeAll { $0.pair == pair }
        entries.append(Entry(pair: pair, style: style))
        if entries.count > 4 { entries.removeFirst(entries.count - 4) }
    }
}
