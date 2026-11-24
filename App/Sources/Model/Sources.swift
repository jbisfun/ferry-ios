// App sources: AltStore-format JSON repositories and GitHub repositories
// (whose releases' .ipa assets become versions). Decoding is lenient because
// sources in the wild mix the v1 and v2 AltStore layouts.
import Foundation
import SwiftUI

struct SourceAppVersion: Hashable, Identifiable {
    var version: String
    var build: String?
    var date: Date?
    var notes: String?
    var downloadURL: URL
    var size: Int64?
    var minOS: String?
    var id: String { downloadURL.absoluteString }
}

struct Permission: Hashable, Identifiable {
    var name: String
    var usage: String?
    var id: String { name }
}

struct SourceApp: Hashable, Identifiable {
    var name: String
    var bundleID: String
    var developer: String
    var subtitle: String?
    var description: String?
    var iconURL: URL?
    var tint: Color?
    var category: String?
    var screenshots: [URL]
    var versions: [SourceAppVersion]
    var entitlements: [Permission]
    var privacy: [Permission]
    var sourceID: String
    var sourceName: String

    var id: String { "\(sourceID)/\(bundleID)" }
    var latest: SourceAppVersion? { versions.first }
}

struct Source: Hashable, Identifiable {
    enum Kind: String, Codable { case altstore, github }
    var url: URL
    var kind: Kind
    var name: String
    var subtitle: String?
    var iconURL: URL?
    var tint: Color?
    var apps: [SourceApp]
    var id: String { url.absoluteString }
}

// MARK: - AltStore JSON

private struct Lenient<T: Decodable>: Decodable {
    var value: T?
    init(from d: Decoder) throws { value = try? T(from: d) }
}

private struct RawSource: Decodable {
    var name: String?
    var identifier: String?
    var subtitle: String?
    var iconURL: String?
    var tintColor: String?
    var apps: [Lenient<RawApp>]?
}

private struct RawScreenshot: Decodable {
    var url: String?
    init(from d: Decoder) throws {
        if let s = try? d.singleValueContainer().decode(String.self) { url = s; return }
        enum K: String, CodingKey { case imageURL }
        url = try? d.container(keyedBy: K.self).decode(String.self, forKey: .imageURL)
    }
}

private struct RawScreenshots: Decodable {
    var urls: [String] = []
    init(from d: Decoder) throws {
        if let list = try? d.singleValueContainer().decode([RawScreenshot].self) {
            urls = list.compactMap(\.url)
        } else if let byDevice = try? d.singleValueContainer().decode([String: [RawScreenshot]].self) {
            urls = (byDevice["iphone"] ?? byDevice.values.first ?? []).compactMap(\.url)
        }
    }
}

private struct RawPermission: Decodable {
    var name: String?
    var usage: String?
    init(from d: Decoder) throws {
        if let s = try? d.singleValueContainer().decode(String.self) { name = s; return }
        enum K: String, CodingKey { case name, type, usageDescription }
        let c = try d.container(keyedBy: K.self)
        name = (try? c.decode(String.self, forKey: .name)) ?? (try? c.decode(String.self, forKey: .type))
        usage = try? c.decode(String.self, forKey: .usageDescription)
    }
}

private struct RawPermissions: Decodable {
    var entitlements: [Permission] = []
    var privacy: [Permission] = []
    init(from d: Decoder) throws {
        enum K: String, CodingKey { case entitlements, privacy }
        // v1: [{type, usageDescription}]
        if let list = try? d.singleValueContainer().decode([RawPermission].self) {
            privacy = list.compactMap { p in p.name.map { Permission(name: $0, usage: p.usage) } }
            return
        }
        let c = try d.container(keyedBy: K.self)
        if let e = try? c.decode([RawPermission].self, forKey: .entitlements) {
            entitlements = e.compactMap { p in p.name.map { Permission(name: $0, usage: p.usage) } }
        }
        if let p = try? c.decode([String: String].self, forKey: .privacy) {
            privacy = p.map { Permission(name: $0.key, usage: $0.value) }.sorted { $0.name < $1.name }
        } else if let p = try? c.decode([RawPermission].self, forKey: .privacy) {
            privacy = p.compactMap { r in r.name.map { Permission(name: $0, usage: r.usage) } }
        }
    }
}

private struct RawVersion: Decodable {
    var version: String?
    var buildVersion: String?
    var date: String?
    var localizedDescription: String?
    var downloadURL: String?
    var size: Int64?
    var minOSVersion: String?
}

private struct RawApp: Decodable {
    var name: String?
    var bundleIdentifier: String?
    var developerName: String?
    var subtitle: String?
    var localizedDescription: String?
    var iconURL: String?
    var tintColor: String?
    var category: String?
    var screenshots: Lenient<RawScreenshots>?
    var screenshotURLs: [String]?
    var versions: [Lenient<RawVersion>]?
    var appPermissions: Lenient<RawPermissions>?
    // v1
    var version: String?
    var versionDate: String?
    var versionDescription: String?
    var downloadURL: String?
    var size: Int64?
}

private func date(_ s: String?) -> Date? {
    guard let s else { return nil }
    if let d = parseISODate(s) { return d }
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.dateFormat = "yyyy-MM-dd"
    return f.date(from: String(s.prefix(10)))
}

extension Color {
    init?(hex: String?) {
        guard var h = hex?.trimmingCharacters(in: .whitespaces) else { return nil }
        if h.hasPrefix("#") { h.removeFirst() }
        guard h.count == 6, let v = UInt32(h, radix: 16) else { return nil }
        self.init(red: Double(v >> 16 & 0xff) / 255, green: Double(v >> 8 & 0xff) / 255, blue: Double(v & 0xff) / 255)
    }
}

enum SourceLoader {
    static func load(_ url: URL) async throws -> Source {
        if let repo = githubRepo(url) { return try await loadGitHub(owner: repo.0, repo: repo.1, url: url) }
        let (data, response) = try await URLSession.shared.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw CoreError(message: "\(url.host() ?? "Source") answered HTTP \(http.statusCode)")
        }
        let raw: RawSource
        do { raw = try JSONDecoder().decode(RawSource.self, from: data) } catch {
            throw CoreError(message: "Not an AltStore source: \(url.absoluteString)")
        }
        let sid = raw.identifier ?? url.absoluteString
        let sname = raw.name ?? url.host() ?? "Source"
        let apps: [SourceApp] = (raw.apps ?? []).compactMap(\.value).compactMap { a in
            guard let name = a.name, let bid = a.bundleIdentifier else { return nil }
            var versions = (a.versions ?? []).compactMap(\.value).compactMap { v -> SourceAppVersion? in
                guard let s = v.downloadURL, let u = URL(string: s) else { return nil }
                return SourceAppVersion(version: v.version ?? "?", build: v.buildVersion, date: date(v.date),
                                        notes: v.localizedDescription, downloadURL: u, size: v.size, minOS: v.minOSVersion)
            }
            if versions.isEmpty, let s = a.downloadURL, let u = URL(string: s) {
                versions = [SourceAppVersion(version: a.version ?? "?", date: date(a.versionDate), notes: a.versionDescription,
                                             downloadURL: u, size: a.size)]
            }
            let perms = a.appPermissions?.value
            return SourceApp(name: name, bundleID: bid, developer: a.developerName ?? sname, subtitle: a.subtitle,
                             description: a.localizedDescription, iconURL: a.iconURL.flatMap(URL.init(string:)),
                             tint: Color(hex: a.tintColor), category: a.category,
                             screenshots: ((a.screenshots?.value?.urls).flatMap { $0.isEmpty ? nil : $0 } ?? a.screenshotURLs ?? [])
                                .compactMap(URL.init(string:)),
                             versions: versions, entitlements: perms?.entitlements ?? [], privacy: perms?.privacy ?? [],
                             sourceID: sid, sourceName: sname)
        }
        return Source(url: url, kind: .altstore, name: sname, subtitle: raw.subtitle,
                      iconURL: raw.iconURL.flatMap(URL.init(string:)), tint: Color(hex: raw.tintColor), apps: apps)
    }

    /// github.com/<owner>/<repo>[/...] or github:owner/repo
    static func githubRepo(_ url: URL) -> (String, String)? {
        if url.scheme == "github" {
            let parts = (url.host().map { [$0] } ?? []) + url.pathComponents.filter { $0 != "/" }
            let p = parts.count >= 2 ? parts : url.absoluteString.dropFirst("github:".count).split(separator: "/").map(String.init)
            return p.count >= 2 ? (p[0], p[1]) : nil
        }
        guard url.host()?.lowercased() == "github.com" else { return nil }
        let p = url.pathComponents.filter { $0 != "/" }
        guard p.count >= 2 else { return nil }
        return (p[0], p[1].replacingOccurrences(of: ".git", with: ""))
    }

    private struct GHRepo: Decodable {
        struct Owner: Decodable { var login: String; var avatar_url: String? }
        var name: String
        var description: String?
        var owner: Owner
    }
    private struct GHRelease: Decodable {
        struct Asset: Decodable { var name: String; var browser_download_url: String; var size: Int64 }
        var tag_name: String
        var name: String?
        var body: String?
        var published_at: String?
        var prerelease: Bool
        var draft: Bool
        var assets: [Asset]
    }

    private static func github<T: Decodable>(_ path: String) async throws -> T {
        var req = URLRequest(url: URL(string: "https://api.github.com\(path)")!)
        req.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let (data, response) = try await URLSession.shared.data(for: req)
        if let http = response as? HTTPURLResponse, http.statusCode == 403 || http.statusCode == 429 {
            throw CoreError(message: "GitHub's rate limit was hit; try again in a few minutes")
        }
        if let http = response as? HTTPURLResponse, http.statusCode == 404 {
            throw CoreError(message: "GitHub repository not found")
        }
        return try JSONDecoder().decode(T.self, from: data)
    }

    static func loadGitHub(owner: String, repo: String, url: URL) async throws -> Source {
        async let info: GHRepo = github("/repos/\(owner)/\(repo)")
        async let releases: [GHRelease] = github("/repos/\(owner)/\(repo)/releases?per_page=20")
        let (r, rels) = try await (info, releases)
        let versions: [SourceAppVersion] = rels.filter { !$0.draft }.flatMap { rel in
            rel.assets.filter { $0.name.lowercased().hasSuffix(".ipa") }.compactMap { a in
                URL(string: a.browser_download_url).map {
                    SourceAppVersion(version: rel.tag_name.trimmingCharacters(in: CharacterSet(charactersIn: "vV")),
                                     date: date(rel.published_at), notes: rel.body, downloadURL: $0, size: a.size)
                }
            }
        }
        let sid = "github:\(owner)/\(repo)"
        let icon = r.owner.avatar_url.flatMap(URL.init(string:))
        let app = SourceApp(name: r.name, bundleID: "github.\(owner).\(repo)", developer: r.owner.login,
                            subtitle: r.description, description: r.description, iconURL: icon, tint: nil, category: nil,
                            screenshots: [], versions: versions, entitlements: [], privacy: [], sourceID: sid,
                            sourceName: "\(owner)/\(repo)")
        return Source(url: url, kind: .github, name: "\(owner)/\(repo)", subtitle: r.description, iconURL: icon,
                      tint: nil, apps: versions.isEmpty ? [] : [app])
    }
}

/// The user's sources: URLs persisted in UserDefaults, contents fetched on demand.
@MainActor @Observable
final class SourceStore {
    struct Failure: Identifiable { var url: URL; var message: String; var id: String { url.absoluteString } }

    private(set) var urls: [URL]
    private(set) var sources: [Source] = []
    private(set) var failures: [Failure] = []
    private(set) var loading = false
    private static let key = "sources"

    /// Offered on the empty Sources screen.
    static let suggestions: [(name: String, url: String)] = [
        ("SideStore", "https://apps.sidestore.io/"),
        ("SideStore Community", "https://community-apps.sidestore.io/sidecommunity.json"),
        ("AltStore", "https://apps.altstore.io/"),
    ]

    init() {
        urls = (UserDefaults.standard.stringArray(forKey: Self.key) ?? []).compactMap(URL.init(string:))
    }

    var allApps: [SourceApp] {
        sources.flatMap(\.apps).sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    static func normalize(_ text: String) -> URL? {
        var t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.hasPrefix("github:") { return URL(string: t) }
        if !t.contains("://") { t = "https://" + t }
        guard let u = URL(string: t), u.host() != nil else { return nil }
        return u
    }

    /// Validates by loading it before saving.
    func add(_ text: String) async throws {
        guard let url = Self.normalize(text) else { throw CoreError(message: "That isn't a URL") }
        guard !urls.contains(url) else { throw CoreError(message: "You already have this source") }
        let s = try await SourceLoader.load(url)
        urls.append(url)
        sources.removeAll { $0.url == url }
        sources.append(s)
        save()
    }

    func remove(_ url: URL) {
        urls.removeAll { $0 == url }
        sources.removeAll { $0.url == url }
        failures.removeAll { $0.url == url }
        save()
    }

    func refresh() async {
        loading = true
        defer { loading = false }
        var loaded: [Source] = []
        var failed: [Failure] = []
        await withTaskGroup(of: (URL, Result<Source, Error>).self) { group in
            for u in urls {
                group.addTask { do { return (u, .success(try await SourceLoader.load(u))) } catch { return (u, .failure(error)) } }
            }
            for await (u, r) in group {
                switch r {
                case .success(let s): loaded.append(s)
                case .failure(let e): failed.append(Failure(url: u, message: e.localizedDescription))
                }
            }
        }
        let order = Dictionary(uniqueKeysWithValues: urls.enumerated().map { ($1, $0) })
        sources = loaded.sorted { (order[$0.url] ?? 0) < (order[$1.url] ?? 0) }
        failures = failed
    }

    private func save() {
        UserDefaults.standard.set(urls.map(\.absoluteString), forKey: Self.key)
    }
}
