import Foundation
import Security

/// The Apple ID is kept in the Keychain so the core can sign in again on
/// each launch (sessions can't be persisted). This device only.
enum Keychain {
    private static let service = "Ferry Apple ID"

    static func save(email: String, password: String) {
        delete()
        let attrs: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: email,
            kSecValueData as String: Data(password.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        SecItemAdd(attrs as CFDictionary, nil)
    }

    static func load() -> (email: String, password: String)? {
        let q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnAttributes as String: true,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let d = out as? [String: Any],
              let email = d[kSecAttrAccount as String] as? String, let data = d[kSecValueData as String] as? Data,
              let password = String(data: data, encoding: .utf8) else { return nil }
        return (email, password)
    }

    static func delete() {
        SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service] as CFDictionary)
    }
}
