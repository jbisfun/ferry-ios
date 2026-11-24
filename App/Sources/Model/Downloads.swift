import Foundation
import Observation

/// IPA downloads with progress, into Caches/Downloads.
@MainActor @Observable
final class Downloads {
    struct Item: Identifiable { let id: URL; var name: String; var fraction: Double }
    private(set) var active: [Item] = []

    static let dir: URL = {
        let d = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("Downloads")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }()

    private final class Delegate: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
        let onProgress: @Sendable (Double) -> Void
        var continuation: CheckedContinuation<URL, Error>?
        let destination: URL
        init(destination: URL, onProgress: @escaping @Sendable (Double) -> Void) {
            self.destination = destination
            self.onProgress = onProgress
        }
        func urlSession(_ s: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64,
                        totalBytesWritten w: Int64, totalBytesExpectedToWrite t: Int64) {
            if t > 0 { onProgress(Double(w) / Double(t)) }
        }
        func urlSession(_ s: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
            if let http = downloadTask.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                continuation?.resume(throwing: CoreError(message: "The server answered HTTP \(http.statusCode)"))
                continuation = nil
                return
            }
            do {
                try? FileManager.default.removeItem(at: destination)
                try FileManager.default.moveItem(at: location, to: destination)
                continuation?.resume(returning: destination)
            } catch { continuation?.resume(throwing: error) }
            continuation = nil
        }
        func urlSession(_ s: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
            if let error { continuation?.resume(throwing: error); continuation = nil }
        }
    }

    func download(_ url: URL, name: String) async throws -> URL {
        if url.isFileURL { return url }
        let safe = name.replacingOccurrences(of: "/", with: "-")
        let dest = Self.dir.appendingPathComponent("\(safe)-\(UUID().uuidString.prefix(8)).ipa")
        active.append(Item(id: url, name: name, fraction: 0))
        defer { active.removeAll { $0.id == url } }
        let delegate = Delegate(destination: dest) { f in
            Task { @MainActor in
                if let i = self.active.firstIndex(where: { $0.id == url }) { self.active[i].fraction = f }
            }
        }
        let session = URLSession(configuration: .default, delegate: delegate, delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        let task = session.downloadTask(with: url)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { c in
                delegate.continuation = c
                task.resume()
            }
        } onCancel: { task.cancel() }
    }
}
