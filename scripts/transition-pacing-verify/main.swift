import Foundation

// A′3's pacing, with fake styles: the growing-file loop and the quality-upgrade
// path re-plan one transition several times, so the rules that keep a re-plan
// from filling the window or erasing its own effect are worth pinning.
//
// Run: scripts/check-transition-pacing.sh

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

func isEffect(_ style: String) -> Bool { style == "effect" }

let pair = TransitionPair(outgoing: "out1", incoming: "in1")
let other = TransitionPair(outgoing: "in1", incoming: "in2")

var pacing = TransitionPacing<String>()
check("a first pair may use an effect", pacing.allowsEffect(for: pair, isEffect: isEffect))

// Three re-plans of the same pair, each noting the effect it chose.
pacing.note("effect", for: pair)
check("a re-plan of an effect pair still sees the budget as free",
      pacing.allowsEffect(for: pair, isEffect: isEffect))
pacing.note("effect", for: pair)
pacing.note("effect", for: pair)
check("three re-plans of one pair leave one entry",
      pacing.entries.count == 1, "entries \(pacing.entries.count)")
check("that entry is the pair's newest style",
      pacing.entries.first?.style == "effect" && pacing.entries.first?.pair == pair)

check("the next pair after one effect may use one (3 in 4)",
      pacing.allowsEffect(for: other, isEffect: isEffect))

// Three effects in a row hold the fourth to a blend.
var run = TransitionPacing<String>()
run.note("effect", for: TransitionPair(outgoing: "r1", incoming: "r2"))
run.note("effect", for: TransitionPair(outgoing: "r2", incoming: "r3"))
check("two effects in a row still allow a third",
      run.allowsEffect(for: TransitionPair(outgoing: "r3", incoming: "r4"), isEffect: isEffect))
run.note("effect", for: TransitionPair(outgoing: "r3", incoming: "r4"))
check("three effects in a row block the fourth",
      !run.allowsEffect(for: TransitionPair(outgoing: "r4", incoming: "r5"), isEffect: isEffect))
check("a re-plan of the third effect does not count against itself",
      run.allowsEffect(for: TransitionPair(outgoing: "r3", incoming: "r4"), isEffect: isEffect))
run.note("blend", for: TransitionPair(outgoing: "r4", incoming: "r5"))
check("a blend after three effects frees the next transition",
      run.allowsEffect(for: TransitionPair(outgoing: "r5", incoming: "r6"), isEffect: isEffect))

// The old bug: a note on every attempt filled the window with the same pair,
// so an effect appeared three transitions in a row and then blocked the window.
var copies = TransitionPacing<String>()
for _ in 0..<4 { copies.note("effect", for: pair) }
check("four attempts on one pair still occupy one slot",
      copies.entries.count == 1, "entries \(copies.entries.count)")

// A re-plan that genuinely becomes a blend replaces the style, not appends.
copies.note("blend", for: pair)
check("a re-plan replaces the pair's style",
      copies.entries.count == 1 && copies.entries.first?.style == "blend")
check("the replaced blend does not block the next pair",
      copies.allowsEffect(for: other, isEffect: isEffect))

// The window is four entries: an effect run older than that has aged out.
var window = TransitionPacing<String>()
let p1 = TransitionPair(outgoing: "a", incoming: "b")
let p2 = TransitionPair(outgoing: "b", incoming: "c")
let p3 = TransitionPair(outgoing: "c", incoming: "d")
let p4 = TransitionPair(outgoing: "d", incoming: "e")
window.note("effect", for: p1)
window.note("blend", for: p2)
window.note("blend", for: p3)
window.note("blend", for: p4)
check("an effect four transitions back has aged out",
      window.allowsEffect(for: TransitionPair(outgoing: "e", incoming: "f"), isEffect: isEffect))

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
