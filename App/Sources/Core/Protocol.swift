// Swift mirror of core/src/protocol.rs. Keep the two in step; ProtocolCheck
// compares `CoreMethod` with the core's own list at launch in debug builds.
import Foundation

enum CoreMethod: String, CaseIterable {
    case settingsUpdate = "settings.update"
    case accountLogin = "account.login"
    case accountLogout = "account.logout"
    case accountCertificates = "account.certificates"
    case accountRevokeCertificate = "account.revoke_certificate"
    case accountAppIds = "account.app_ids"
    case accountDeleteAppId = "account.delete_app_id"
    case deviceConnect = "device.connect"
    case deviceApps = "device.apps"
    case deviceUninstall = "device.uninstall"
    case ipaInspect = "ipa.inspect"
    case appInstall = "app.install"
    case libraryList = "library.list"
    case libraryRefresh = "library.refresh"
    case libraryRemove = "library.remove"
}

struct CoreConfig: Codable {
    var data_dir: String
    var tmp_dir: String
    var anisette_mode: String
    var anisette_url: String
    var device_address: String
}

struct InstallOptions: Codable, Hashable {
    var display_name: String?
    var bundle_id: String?
    var version: String?
    var remove_extensions = false
    var inject: [String] = []
    var remove_load_commands: [String] = []
    var enable_file_sharing = false
    var force_all_devices = false
    var minimum_os: String?
}

struct AccountInfo: Codable, Hashable {
    var email: String
    var name: String
    var team_id: String
    var team_name: String
    var free: Bool
}

struct DeveloperItem: Codable, Hashable, Identifiable {
    var id: String
    var name: String
    var detail: String
    var expires: String?
}

struct DeviceInfo: Codable, Hashable {
    var udid: String
    var name: String
    var product_type: String
    var os_version: String
}

struct InstalledApp: Codable, Hashable, Identifiable {
    var bundle_id: String
    var name: String
    var version: String
    var sideloaded: Bool
    var id: String { bundle_id }
}

struct BinaryInfo: Codable, Hashable, Identifiable {
    var path: String
    var kind: String
    var entitlements: JSONValue
    var load_commands: [String]
    var id: String { path }
}

struct IpaInfo: Codable, Hashable {
    var name: String
    var bundle_id: String
    var version: String
    var build: String
    var minimum_os: String
    var icon_path: String?
    var size: UInt64
    var binaries: [BinaryInfo]
    var extensions: [String]
}

struct LibraryApp: Codable, Hashable, Identifiable {
    var original_bundle_id: String
    var installed_bundle_id: String
    var name: String
    var version: String
    var icon_path: String?
    var installed_at: String
    var expires_at: String?
    var team_id: String
    var granted_entitlements: JSONValue
    var options: InstallOptions
    var id: String { original_bundle_id }

    var expiryDate: Date? { expires_at.flatMap(parseISODate) }
}

struct RefreshResult: Codable {
    struct Failure: Codable { var bundle_id: String; var message: String }
    var refreshed: [String]
    var failed: [Failure]
}

struct Empty: Codable {}

// MARK: events

struct Phone: Codable, Hashable, Identifiable {
    var id: UInt32
    var number: String
}

enum PromptRequest: Decodable {
    case twoFactor(method: String, phones: [Phone])
    case revokeCertificates([String])
    case pairingPin

    private enum K: String, CodingKey { case kind, method, phones, certificates }
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: K.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "two_factor":
            self = .twoFactor(method: try c.decode(String.self, forKey: .method), phones: try c.decode([Phone].self, forKey: .phones))
        case "pairing_pin":
            self = .pairingPin
        default:
            self = .revokeCertificates(try c.decode([String].self, forKey: .certificates))
        }
    }
}

struct PromptAnswer: Encodable {
    var code: String?
    var sms: UInt32?
    var revoke: Bool?
    var cancel = false
}

enum CoreEvent: Decodable {
    case result(id: UInt64, data: Data)
    case error(id: UInt64, message: String)
    case progress(id: UInt64, stage: String, fraction: Double?)
    case prompt(id: UInt64, prompt: UInt64, request: PromptRequest)
    case log(level: String, message: String)

    private enum K: String, CodingKey { case type, id, result, message, stage, fraction, prompt, request, level }
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: K.self)
        switch try c.decode(String.self, forKey: .type) {
        case "result":
            let v = try c.decode(JSONValue.self, forKey: .result)
            self = .result(id: try c.decode(UInt64.self, forKey: .id), data: try JSONEncoder().encode(v))
        case "error":
            self = .error(id: try c.decode(UInt64.self, forKey: .id), message: try c.decode(String.self, forKey: .message))
        case "progress":
            self = .progress(id: try c.decode(UInt64.self, forKey: .id), stage: try c.decode(String.self, forKey: .stage),
                             fraction: try c.decodeIfPresent(Double.self, forKey: .fraction))
        case "prompt":
            self = .prompt(id: try c.decode(UInt64.self, forKey: .id), prompt: try c.decode(UInt64.self, forKey: .prompt),
                           request: try c.decode(PromptRequest.self, forKey: .request))
        default:
            self = .log(level: (try? c.decode(String.self, forKey: .level)) ?? "info",
                        message: (try? c.decode(String.self, forKey: .message)) ?? "")
        }
    }
}

/// Arbitrary JSON (entitlement values, raw results).
enum JSONValue: Codable, Hashable {
    case null, bool(Bool), number(Double), string(String), array([JSONValue]), object([String: JSONValue])

    init(from d: Decoder) throws {
        let c = try d.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    func encode(to e: Encoder) throws {
        var c = e.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    var object: [String: JSONValue] { if case .object(let o) = self { o } else { [:] } }

    /// One-line rendering for lists.
    var summary: String {
        switch self {
        case .null: "—"
        case .bool(let b): b ? "Yes" : "No"
        case .number(let n): n == n.rounded() ? String(Int(n)) : String(n)
        case .string(let s): s
        case .array(let a): a.map(\.summary).joined(separator: ", ")
        case .object(let o): "\(o.count) keys"
        }
    }
}

func parseISODate(_ s: String) -> Date? {
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let d = f.date(from: s) { return d }
    f.formatOptions = [.withInternetDateTime]
    return f.date(from: s)
}
