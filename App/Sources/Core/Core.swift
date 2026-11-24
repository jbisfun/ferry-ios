// Async Swift face of the Rust core. One C callback receives every event;
// results resume the matching continuation, progress goes to the request's
// handler, and prompts go to the app model (2FA sheet, revoke confirmation).
import Foundation

struct CoreError: LocalizedError {
    var message: String
    var errorDescription: String? { message }
    var isCancelled: Bool { message == "Cancelled" }
}

struct CoreProgress: Equatable {
    var stage: String
    var fraction: Double?
}

final class Core: @unchecked Sendable {
    static let shared = Core()

    private let lock = NSLock()
    private var waiting: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var progressHandlers: [UInt64: @Sendable (CoreProgress) -> Void] = [:]
    /// Early events for an id whose continuation isn't registered yet.
    private var early: [UInt64: Result<Data, Error>] = [:]
    private(set) var started = false

    /// Set by the app model; called on the main actor.
    @MainActor var onPrompt: ((UInt64, PromptRequest) -> Void)?

    static let dataDir: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Ferry", isDirectory: true)
    }()
    /// Core debug log, in Documents so it can be shared (Files, Settings → Log).
    static let logFile: URL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("ferry-core.log")
    static let tmpDir: URL = FileManager.default.temporaryDirectory.appendingPathComponent("FerryCore", isDirectory: true)

    func start(anisetteMode: String, anisetteURL: String, deviceAddress: String) {
        guard !started else { return }
        try? FileManager.default.removeItem(at: Self.tmpDir)
        let config = CoreConfig(data_dir: Self.dataDir.path, tmp_dir: Self.tmpDir.path,
                                anisette_mode: anisetteMode, anisette_url: anisetteURL, device_address: deviceAddress,
                                log_path: Self.logFile.path)
        let json = String(data: try! JSONEncoder().encode(config), encoding: .utf8)!
        started = ferry_core_start(json, { cString, _ in
            guard let cString else { return }
            Core.shared.handle(Data(bytes: cString, count: strlen(cString)))
        }, nil)
        #if DEBUG
        checkProtocol()
        #endif
    }

    private func handle(_ json: Data) {
        guard let event = try? JSONDecoder().decode(CoreEvent.self, from: json) else { return }
        switch event {
        case .result(let id, let data): finish(id, .success(data))
        case .error(let id, let message): finish(id, .failure(CoreError(message: message)))
        case .progress(let id, let stage, let fraction):
            let h = lock.withLock { progressHandlers[id] }
            h?(CoreProgress(stage: stage, fraction: fraction))
        case .prompt(_, let prompt, let request):
            Task { @MainActor in
                if let onPrompt = self.onPrompt { onPrompt(prompt, request) } else { self.respond(prompt, PromptAnswer(cancel: true)) }
            }
        case .log(let level, let message):
            Log.shared.append(level: level, message)
        }
    }

    private func finish(_ id: UInt64, _ result: Result<Data, Error>) {
        lock.lock()
        progressHandlers[id] = nil
        if let c = waiting.removeValue(forKey: id) {
            lock.unlock()
            c.resume(with: result)
        } else {
            early[id] = result
            lock.unlock()
        }
    }

    func respond(_ prompt: UInt64, _ answer: PromptAnswer) {
        let json = String(data: try! JSONEncoder().encode(answer), encoding: .utf8)!
        _ = ferry_core_respond(prompt, json)
    }

    private struct Envelope<P: Encodable>: Encodable {
        var method: String
        var params: P?
    }

    /// Run one request. Cancelling the Swift task cancels it in the core.
    func call<R: Decodable>(_ method: CoreMethod, _ params: (some Encodable)? = Empty?.none,
                            as: R.Type = R.self,
                            progress: (@Sendable (CoreProgress) -> Void)? = nil) async throws -> R {
        guard started else { throw CoreError(message: "Ferry's core didn't start") }
        let body = try JSONEncoder().encode(Envelope(method: method.rawValue, params: params))
        let id = ferry_core_request(String(data: body, encoding: .utf8)!)
        if let progress { lock.withLock { progressHandlers[id] = progress } }
        let data: Data = try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { c in
                lock.lock()
                if let r = early.removeValue(forKey: id) {
                    lock.unlock()
                    c.resume(with: r)
                } else {
                    waiting[id] = c
                    lock.unlock()
                }
            }
        } onCancel: {
            ferry_core_cancel(id)
        }
        if R.self == Empty.self { return Empty() as! R }
        return try JSONDecoder().decode(R.self, from: data)
    }

    private func checkProtocol() {
        guard let p = ferry_core_methods() else { return }
        defer { ferry_core_free(p) }
        let core = (try? JSONDecoder().decode([String].self, from: Data(String(cString: p).utf8))) ?? []
        let swift = CoreMethod.allCases.map(\.rawValue)
        assert(Set(core) == Set(swift), "Protocol.swift is out of step with protocol.rs: core \(core), swift \(swift)")
    }
}

/// Recent core log lines, shown in Settings → Diagnostics.
@MainActor @Observable
final class Log {
    nonisolated static let shared = Log()
    nonisolated init() {}
    struct Line: Identifiable { let id = UUID(); let date = Date(); let level: String; let text: String }
    private(set) var lines: [Line] = []

    nonisolated func append(level: String, _ text: String) {
        Task { @MainActor in
            self.lines.append(Line(level: level, text: text))
            if self.lines.count > 500 { self.lines.removeFirst(self.lines.count - 500) }
        }
    }
}
