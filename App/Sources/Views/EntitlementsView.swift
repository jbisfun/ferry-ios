import SwiftUI

/// Human names for entitlement keys and privacy usage strings.
enum Entitlements {
    static let names: [String: (String, String)] = [
        "application-identifier": ("App Identifier", "number"),
        "com.apple.developer.team-identifier": ("Team", "person.2"),
        "get-task-allow": ("Debuggable", "ant"),
        "keychain-access-groups": ("Keychain Sharing", "key"),
        "com.apple.security.application-groups": ("App Groups", "square.on.square"),
        "aps-environment": ("Push Notifications", "bell.badge"),
        "com.apple.developer.associated-domains": ("Associated Domains", "link"),
        "com.apple.developer.icloud-container-identifiers": ("iCloud Containers", "icloud"),
        "com.apple.developer.icloud-services": ("iCloud", "icloud"),
        "com.apple.developer.ubiquity-kvstore-identifier": ("iCloud Key-Value Store", "icloud"),
        "com.apple.developer.healthkit": ("HealthKit", "heart"),
        "com.apple.developer.homekit": ("HomeKit", "house"),
        "com.apple.developer.siri": ("Siri", "waveform"),
        "com.apple.developer.networking.networkextension": ("Network Extensions", "network"),
        "com.apple.developer.networking.vpn.api": ("Personal VPN", "lock.shield"),
        "com.apple.developer.networking.wifi-info": ("Wi-Fi Info", "wifi"),
        "com.apple.developer.nfc.readersession.formats": ("NFC", "wave.3.right"),
        "com.apple.developer.in-app-payments": ("Apple Pay", "creditcard"),
        "com.apple.developer.applesignin": ("Sign in with Apple", "applelogo"),
        "com.apple.developer.kernel.increased-memory-limit": ("Increased Memory Limit", "memorychip"),
        "com.apple.developer.kernel.extended-virtual-addressing": ("Extended Virtual Addressing", "memorychip"),
        "com.apple.developer.game-center": ("Game Center", "gamecontroller"),
        "com.apple.developer.usernotifications.time-sensitive": ("Time-Sensitive Notifications", "bell"),
        "com.apple.developer.usernotifications.communication": ("Communication Notifications", "message"),
        "com.apple.developer.default-data-protection": ("Data Protection", "lock"),
        "com.apple.developer.carplay-audio": ("CarPlay Audio", "car"),
        "com.apple.developer.weatherkit": ("WeatherKit", "cloud.sun"),
        "com.apple.developer.family-controls": ("Family Controls", "figure.and.child.holdinghands"),
        "com.apple.developer.devicecheck.appattest-environment": ("App Attest", "checkmark.seal"),
        "com.apple.external-accessory.wireless-configuration": ("Accessory Setup", "cable.connector"),
    ]

    /// Free ("Personal Team") profiles can carry these; anything else is dropped.
    static let freeKeys: Set<String> = [
        "application-identifier", "com.apple.developer.team-identifier", "get-task-allow", "keychain-access-groups",
        "com.apple.security.application-groups", "com.apple.developer.kernel.increased-memory-limit",
        "com.apple.developer.kernel.extended-virtual-addressing", "com.apple.developer.healthkit",
        "com.apple.developer.homekit", "com.apple.developer.siri", "com.apple.developer.networking.wifi-info",
        "com.apple.developer.default-data-protection", "com.apple.developer.associated-domains",
        "com.apple.developer.healthkit.access",
    ]

    static func freeAllowed(_ key: String) -> Bool { freeKeys.contains(key) }
    static func name(_ key: String) -> String { names[key]?.0 ?? key }
    static func icon(_ key: String) -> String { names[key]?.1 ?? "checkmark.shield" }

    static func privacyName(_ key: String) -> String {
        var k = key.replacingOccurrences(of: "NS", with: "").replacingOccurrences(of: "UsageDescription", with: "")
        k = k.replacingOccurrences(of: "AlwaysAndWhenInUse", with: "")
        // "PhotoLibraryAdd" -> "Photo Library Add"
        return k.reduce(into: "") { s, c in
            if c.isUppercase, let last = s.last, last.isLowercase { s.append(" ") }
            s.append(c)
        }
    }

    static func privacyIcon(_ key: String) -> String {
        let k = key.lowercased()
        if k.contains("camera") { return "camera" }
        if k.contains("microphone") { return "mic" }
        if k.contains("photo") { return "photo" }
        if k.contains("location") { return "location" }
        if k.contains("contacts") { return "person.crop.circle" }
        if k.contains("bluetooth") { return "dot.radiowaves.left.and.right" }
        if k.contains("localnetwork") { return "network" }
        if k.contains("calendar") { return "calendar" }
        if k.contains("faceid") { return "faceid" }
        return "hand.raised"
    }
}

struct EntitlementRow: View {
    let key: String
    let value: JSONValue?
    var restricted = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: Entitlements.icon(key)).frame(width: 24).foregroundStyle(restricted ? .orange : .accentColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(Entitlements.name(key)).font(.subheadline.weight(.medium))
                Text(key).font(.caption2.monospaced()).foregroundStyle(.secondary)
                if let value, case .array(let items) = value, !items.isEmpty {
                    ForEach(Array(items.prefix(6).enumerated()), id: \.offset) { _, v in
                        Text(v.summary).font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                } else if let value {
                    Text(value.summary).font(.caption.monospaced()).foregroundStyle(.secondary)
                }
                if restricted {
                    Text("Not available to free Apple IDs").font(.caption2).foregroundStyle(.orange)
                }
            }
        }
        .padding(.vertical, 2)
    }
}

struct EntitlementsList: View {
    let title: String
    let entitlements: [String: JSONValue]
    var free = false

    var body: some View {
        List {
            if entitlements.isEmpty {
                ContentUnavailableView("No Entitlements", systemImage: "checkmark.shield")
            }
            ForEach(entitlements.keys.sorted(), id: \.self) { k in
                EntitlementRow(key: k, value: entitlements[k], restricted: free && !Entitlements.freeAllowed(k))
            }
        }
        .navigationTitle(title)
    }
}

/// Every binary in the IPA and what it requests, plus what the source declared.
struct EntitlementsOverview: View {
    let binaries: [BinaryInfo]
    let declared: [Permission]
    let free: Bool

    var body: some View {
        List {
            ForEach(binaries) { b in
                Section {
                    let e = b.entitlements.object
                    if e.isEmpty {
                        Text("None").foregroundStyle(.secondary)
                    }
                    ForEach(e.keys.sorted(), id: \.self) { k in
                        EntitlementRow(key: k, value: e[k], restricted: free && !Entitlements.freeAllowed(k))
                    }
                } header: {
                    Label(b.path, systemImage: icon(b.kind)).textCase(nil)
                }
            }
            if !declared.isEmpty {
                Section("Declared by the source") {
                    ForEach(declared) { EntitlementRow(key: $0.name, value: nil) }
                }
            }
        }
        .navigationTitle("Entitlements")
    }

    private func icon(_ kind: String) -> String {
        switch kind {
        case "extension": "puzzlepiece.extension"
        case "framework": "shippingbox"
        case "dylib": "gearshape.2"
        default: "app"
        }
    }
}
