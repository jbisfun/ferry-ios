import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var importingPairing = false

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            Form {
                Section("Apple ID") {
                    if let a = model.account {
                        NavigationLink { AccountView(account: a) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle.fill").font(.largeTitle).foregroundStyle(.tint)
                                VStack(alignment: .leading) {
                                    Text(a.name.isEmpty ? a.email : a.name).font(.headline)
                                    Text(a.free ? "\(a.team_name) · Free" : a.team_name).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    } else {
                        NavigationLink { SignInView() } label: {
                            Label(model.signingIn ? "Signing In…" : "Sign In", systemImage: "person.crop.circle.badge.plus")
                        }
                    }
                }

                Section {
                    HStack {
                        Label("Connection", systemImage: "iphone.radiowaves.left.and.right")
                        Spacer()
                        switch model.device {
                        case .connected: Text("Connected").foregroundStyle(.green)
                        case .checking: ProgressView()
                        case .failed: Text("Not connected").foregroundStyle(.orange)
                        case .unknown: Text("—").foregroundStyle(.secondary)
                        }
                    }
                    if case .connected(let d) = model.device {
                        LabeledContent("Device", value: d.name)
                        LabeledContent("iOS", value: d.os_version)
                    }
                    if case .failed(let m) = model.device {
                        Text(m).font(.caption).foregroundStyle(.secondary)
                    }
                    Button("Check Connection") { Task { await model.checkDevice() } }
                    Button(model.hasPairingFile ? "Replace Pairing File…" : "Import Pairing File…") { importingPairing = true }
                    NavigationLink("Setup Guide") { SetupGuide() }
                } header: {
                    Text("This iPhone")
                } footer: {
                    Text("Ferry installs apps through a loopback VPN (LocalDevVPN or StosVPN) and this iPhone's pairing file. No computer or JIT is needed after setup.")
                }

                Section {
                    Picker("Anisette", selection: $model.anisetteMode) {
                        Text("On This iPhone").tag("local")
                        Text("Remote Server").tag("remote")
                    }
                } footer: {
                    Text(model.anisetteMode == "local"
                         ? "Ferry vouches for this iPhone itself by running Apple's anisette code locally (no JIT, no third-party server). The first sign-in downloads it from Apple and takes a little longer."
                         : "A remote server vouches for this iPhone when signing in to Apple.")
                }

                Section {
                    if model.anisetteMode == "remote" {
                    Picker("Anisette Server", selection: $model.anisetteURL) {
                        ForEach(AppModel.anisetteServers, id: \.url) { Text($0.name).tag($0.url) }
                        if !AppModel.anisetteServers.contains(where: { $0.url == model.anisetteURL }) {
                            Text(model.anisetteURL).tag(model.anisetteURL)
                        }
                    }
                    TextField("Custom server", text: $model.anisetteURL)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                        .font(.callout.monospaced())
                    }
                    TextField("Device address", text: $model.deviceAddress)
                        .keyboardType(.numbersAndPunctuation).font(.callout.monospaced())
                } header: {
                    Text("Advanced")
                } footer: {
                    Text("The device address is where the VPN routes this iPhone's lockdown service (10.7.0.1 for LocalDevVPN/StosVPN).")
                }

                Section {
                    Toggle("Remind Before Apps Expire", isOn: $model.notifyBeforeExpiry)
                    NavigationLink("Log") { LogView() }
                } footer: {
                    Text("Ferry \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")")
                }
            }
            .navigationTitle("Settings")
            .fileImporter(isPresented: $importingPairing, allowedContentTypes: [.propertyList, .xml, .data]) { r in
                guard case .success(let url) = r else { return }
                do { try model.importPairingFile(from: url) } catch { model.show(error, title: "Couldn't import the pairing file") }
            }
        }
    }
}

struct SignInView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var email = Keychain.load()?.email ?? ""
    @State private var password = ""

    var body: some View {
        Form {
            Section {
                TextField("Email", text: $email)
                    .textContentType(.username).keyboardType(.emailAddress)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField("Password", text: $password).textContentType(.password)
            } footer: {
                Text("Your Apple ID is sent only to Apple, and kept in this iPhone's Keychain so Ferry can refresh apps. A free Apple ID works; consider a secondary one.")
            }
            Section {
                Button {
                    Task {
                        await model.signIn(email: email, password: password)
                        if model.account != nil { dismiss() }
                    }
                } label: {
                    HStack { Spacer(); if model.signingIn { ProgressView() } else { Text("Sign In").bold() }; Spacer() }
                }
                .disabled(email.isEmpty || password.isEmpty || model.signingIn)
            }
        }
        .navigationTitle("Sign In")
    }
}

struct AccountView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let account: AccountInfo
    @State private var certificates: [DeveloperItem]?
    @State private var appIDs: [DeveloperItem]?

    var body: some View {
        List {
            Section {
                LabeledContent("Email", value: account.email)
                LabeledContent("Team", value: account.team_name)
                LabeledContent("Team ID", value: account.team_id)
                LabeledContent("Membership", value: account.free ? "Free" : "Apple Developer Program")
            }
            Section {
                if let certificates {
                    ForEach(certificates) { c in
                        item(c)
                            .swipeActions { Button("Revoke", role: .destructive) { revoke(c) } }
                    }
                    if certificates.isEmpty { Text("None").foregroundStyle(.secondary) }
                } else { ProgressView() }
            } header: { Text("Certificates") } footer: {
                Text("Revoking a certificate stops apps signed with it from opening until they're refreshed.")
            }
            Section {
                if let appIDs {
                    ForEach(appIDs) { item($0) }
                    if appIDs.isEmpty { Text("None").foregroundStyle(.secondary) }
                } else { ProgressView() }
            } header: { Text("App IDs") } footer: {
                if account.free { Text("Free accounts can create 10 App IDs per 7 days.") }
            }
            Section {
                Button("Sign Out", role: .destructive) { Task { await model.signOut(); dismiss() } }
            }
        }
        .navigationTitle("Apple ID")
        .task { await load() }
        .refreshable { await load() }
    }

    private func item(_ i: DeveloperItem) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(i.name).font(.subheadline.weight(.medium))
            Text(i.detail).font(.caption).foregroundStyle(.secondary)
            if let e = i.expires { Text("Expires \(e)").font(.caption2).foregroundStyle(.tertiary) }
        }
    }

    private func load() async {
        async let c: [DeveloperItem] = Core.shared.call(.accountCertificates)
        async let a: [DeveloperItem] = Core.shared.call(.accountAppIds)
        do { certificates = try await c } catch { certificates = []; model.show(error) }
        do { appIDs = try await a } catch { appIDs = [] }
    }

    private func revoke(_ c: DeveloperItem) {
        Task {
            struct P: Encodable { var id: String }
            do { _ = try await Core.shared.call(.accountRevokeCertificate, P(id: c.id), as: Empty.self); await load() }
            catch { model.show(error) }
        }
    }
}

struct SetupGuide: View {
    var body: some View {
        List {
            step(1, "Make a pairing file", "On a computer, pair this iPhone and export its pairing file, e.g. with Jimpactor (Device → Export Pairing), jitterbugpair or idevice_pair. AirDrop or save it to Files.")
            step(2, "Import it", "Settings → Import Pairing File. It stays in Ferry's container.")
            step(3, "Install a loopback VPN", "LocalDevVPN or StosVPN from the App Store. Turn it on whenever you install or refresh.")
            step(4, "Sign in", "Use any Apple ID. Ferry creates a development certificate and registers this iPhone.")
            step(5, "Install apps", "From Browse, or open an .ipa with Ferry. Refresh free-account apps within 7 days.")
        }
        .navigationTitle("Setup")
    }

    private func step(_ n: Int, _ title: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Text("\(n)").font(.headline).frame(width: 28, height: 28).background(Circle().fill(.tint.opacity(0.2)))
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(text).font(.subheadline).foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}

struct LogView: View {
    var body: some View {
        List(Log.shared.lines.reversed()) { l in
            VStack(alignment: .leading, spacing: 2) {
                Text(l.text).font(.caption.monospaced()).textSelection(.enabled)
                Text("\(l.level) · \(l.date.formatted(date: .omitted, time: .standard))").font(.caption2).foregroundStyle(.secondary)
            }
        }
        .overlay { if Log.shared.lines.isEmpty { ContentUnavailableView("No Log Yet", systemImage: "doc.text") } }
        .navigationTitle("Log")
    }
}
