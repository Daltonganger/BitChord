import Foundation
import BitChordShared

/// Discord user gateway — IDENTIFY + OP 3 presence, heartbeat from HELLO.
///
/// Discord drops gateway connections as a matter of course: it asks clients to
/// reconnect (op 7), invalidates sessions (op 9), and a Mac that sleeps or changes
/// network loses the socket without either. The connection is therefore treated as
/// something to keep alive rather than open once — every way it can end leads back
/// to a fresh connect, and the track playing is sent again once the new session is
/// READY, so presence does not quietly stop until the next relaunch.
final class DiscordGateway: NSObject, URLSessionWebSocketDelegate {
    static let shared = DiscordGateway()
    private var session: URLSession?
    private var task: URLSessionWebSocketTask?
    private var heartbeat: Timer?
    private var reconnectTimer: Timer?
    private var seq: Int64?
    private var token: String?
    /// Presence sent before IDENTIFY is answered gets the socket closed with 4003,
    /// so updates wait for READY.
    private var ready = false
    /// No ACK for the previous beat means the connection is a zombie.
    private var awaitingAck = false
    /// The latest presence, re-sent after every READY.
    private var lastPresence: String?
    private var failures = 0
    /// Bumped on every connect, so callbacks from a socket already replaced are ignored.
    private var generation = 0

    /// Close codes after which reconnecting cannot help: a bad token or a
    /// rejected IDENTIFY will be rejected again.
    private static let fatalCloseCodes: Set<Int> = [4004, 4010, 4011, 4012, 4013, 4014]

    func connect(token: String) {
        disconnect()
        self.token = token
        failures = 0
        open()
    }

    func disconnect() {
        token = nil
        lastPresence = nil
        tearDown()
    }

    func updatePresence(title: String, artist: String, album: String?, positionMs: Int64, durationMs: Int64, speed: Float, videoId: String?) {
        guard token != nil else { return }
        let payload = DiscordBridge.shared.presencePayload(
            title: title, artist: artist, album: album,
            positionMs: positionMs, durationMs: durationMs, speed: speed, videoId: videoId
        )
        lastPresence = payload
        if ready { send(payload) }
    }

    private func open() {
        tearDown()
        guard token != nil else { return }
        generation += 1
        let session = URLSession(configuration: .default, delegate: self, delegateQueue: .main)
        let url = URL(string: "wss://gateway.discord.gg/?v=9&encoding=json")!
        let t = session.webSocketTask(with: url)
        self.session = session
        task = t
        t.resume()
        listen(generation)
    }

    private func tearDown() {
        heartbeat?.invalidate()
        heartbeat = nil
        reconnectTimer?.invalidate()
        reconnectTimer = nil
        ready = false
        awaitingAck = false
        seq = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        session?.finishTasksAndInvalidate()
        session = nil
    }

    /// Reconnects after a growing pause — 2s, 4s, 8s… up to a minute — so an outage
    /// is not met with a connection attempt every heartbeat.
    private func scheduleReconnect(closeCode: Int? = nil) {
        if let closeCode, Self.fatalCloseCodes.contains(closeCode) {
            DebugLog.shared.w(message: "Discord closed the gateway with \(closeCode); not reconnecting")
            disconnect()
            return
        }
        guard token != nil, reconnectTimer == nil else { return }
        tearDown()
        failures += 1
        let delay = min(60, pow(2, Double(min(failures, 6))))
        reconnectTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) { [weak self] _ in
            self?.reconnectTimer = nil
            self?.open()
        }
    }

    private func listen(_ generation: Int) {
        task?.receive { [weak self] result in
            guard let self, generation == self.generation else { return }
            switch result {
            case .failure:
                self.scheduleReconnect(closeCode: self.task?.closeCode.rawValue)
            case .success(let message):
                if case .string(let text) = message {
                    self.handle(text)
                }
                if generation == self.generation { self.listen(generation) }
            }
        }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask,
                    didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        guard webSocketTask === task else { return }
        scheduleReconnect(closeCode: closeCode.rawValue)
    }

    private func handle(_ text: String) {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        if let s = obj["s"] as? Int { seq = Swift.Int64(s) }
        switch obj["op"] as? Int {
        case 10:
            guard let d = obj["d"] as? [String: Any], let interval = d["heartbeat_interval"] as? Double,
                  let token else { return }
            startHeartbeat(interval / 1000)
            send(DiscordBridge.shared.identifyPayload(token: token))
        case 0 where obj["t"] as? String == "READY":
            ready = true
            failures = 0
            if let lastPresence { send(lastPresence) }
        case 1:
            beat()
        case 11:
            awaitingAck = false
        case 7, 9:
            // Reconnect (7) or invalid session (9). This client never resumes, so
            // both mean a fresh IDENTIFY on a new socket.
            scheduleReconnect()
        default:
            break
        }
    }

    private func startHeartbeat(_ seconds: Double) {
        heartbeat?.invalidate()
        awaitingAck = false
        heartbeat = Timer.scheduledTimer(withTimeInterval: max(5, seconds), repeats: true) { [weak self] _ in
            guard let self else { return }
            if self.awaitingAck {
                self.scheduleReconnect()
                return
            }
            self.beat()
        }
    }

    private func beat() {
        awaitingAck = true
        send(DiscordBridge.shared.heartbeatPayload(seq: seq ?? 0))
    }

    private func send(_ text: String) {
        task?.send(.string(text)) { _ in }
    }
}
