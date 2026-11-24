import SwiftUI
import UniformTypeIdentifiers

/// Inspect an IPA, choose how to change it, then install.
struct InstallSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let pending: PendingInstall

    @State private var info: IpaInfo?
    @State private var loadError: String?
    @State private var options = InstallOptions()
    @State private var pickingTweak = false
    @State private var advanced = false

    var body: some View {
        NavigationStack {
            Group {
                if let info { form(info) }
                else if let loadError {
                    ContentUnavailableView("Can't Read This App", systemImage: "exclamationmark.triangle", description: Text(loadError))
                } else {
                    ProgressView("Reading app…")
                }
            }
            .navigationTitle("Install")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { cleanup(); dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Install") { install() }.bold().disabled(info == nil)
                }
            }
            .fileImporter(isPresented: $pickingTweak, allowedContentTypes: [.dylib, .framework, .item], allowsMultipleSelection: true) { r in
                if case .success(let urls) = r { addTweaks(urls) }
            }
        }
        .task { await load() }
        .interactiveDismissDisabled(info != nil)
    }

    private func load() async {
        let scoped = pending.ipa.startAccessingSecurityScopedResource()
        defer { if scoped { pending.ipa.stopAccessingSecurityScopedResource() } }
        // Files picked in place must be copied so the core (and refreshes) can read them.
        var url = pending.ipa
        if !pending.temporary {
            let dest = Downloads.dir.appendingPathComponent(url.lastPathComponent)
            try? FileManager.default.removeItem(at: dest)
            do { try FileManager.default.copyItem(at: url, to: dest); url = dest } catch {
                loadError = error.localizedDescription; return
            }
        }
        localIPA = url
        struct P: Encodable { var path: String }
        do { info = try await Core.shared.call(.ipaInspect, P(path: url.path)) } catch { loadError = error.localizedDescription }
    }

    @State private var localIPA: URL?

    private var mainBinary: BinaryInfo? { info?.binaries.first }
    private var freeAccount: Bool { model.account?.free ?? true }

    @ViewBuilder
    private func form(_ info: IpaInfo) -> some View {
        Form {
            Section {
                HStack(spacing: 14) {
                    AppIcon(url: info.icon_path.map(URL.init(fileURLWithPath:)) ?? pending.sourceApp?.iconURL, size: 64)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(info.name).font(.title3.bold())
                        Text("\(info.version) (\(info.build))").font(.subheadline).foregroundStyle(.secondary)
                        Text(Int64(info.size).byteString).font(.caption).foregroundStyle(.tertiary)
                    }
                }
            }

            Section {
                TextField(info.name, text: binding(\.display_name))
                TextField(info.bundle_id, text: binding(\.bundle_id))
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .font(.callout.monospaced())
            } header: {
                Text("Name & Identifier")
            } footer: {
                if let team = model.account?.team_id {
                    Text("Installs as \(installedID(info, team: team)).")
                } else {
                    Text("Sign in under Settings to install.").foregroundStyle(.red)
                }
            }

            if !info.extensions.isEmpty {
                Section {
                    Toggle("Remove App Extensions", isOn: $options.remove_extensions)
                } footer: {
                    Text("This app has \(info.extensions.count) extension\(info.extensions.count == 1 ? "" : "s") (widgets, share sheets…). \(freeAccount ? "On a free account each one uses an App ID, of 10 per week." : "")")
                }
            }

            Section {
                ForEach(options.inject, id: \.self) { path in
                    Label(URL(fileURLWithPath: path).lastPathComponent, systemImage: path.hasSuffix(".framework") ? "shippingbox" : "puzzlepiece.extension")
                        .font(.callout.monospaced())
                }
                .onDelete { options.inject.remove(atOffsets: $0) }
                Button { pickingTweak = true } label: { Label("Add Tweak…", systemImage: "plus") }
            } header: {
                Text("Inject")
            } footer: {
                Text(".dylib files and .framework folders are copied into the app and loaded at launch. Tweaks that need Cydia Substrate also need ElleKit injected alongside them.")
            }

            Section {
                NavigationLink {
                    EntitlementsOverview(binaries: info.binaries, declared: pending.sourceApp?.entitlements ?? [], free: freeAccount)
                } label: {
                    LabeledContent("Entitlements", value: "\(mainBinary?.entitlements.object.count ?? 0)")
                }
                if let main = mainBinary {
                    NavigationLink {
                        LoadCommandsView(commands: main.load_commands, removed: $options.remove_load_commands)
                    } label: {
                        LabeledContent("Linked Libraries", value: "\(main.load_commands.count)")
                    }
                }
            } header: {
                Text("What It Asks For")
            } footer: {
                if freeAccount, let n = restrictedCount(info), n > 0 {
                    Text("\(n) entitlement\(n == 1 ? "" : "s") can't be granted to a free Apple ID; the app may lose those features.")
                }
            }

            Section {
                DisclosureGroup("Advanced", isExpanded: $advanced) {
                    TextField("Version (\(info.version))", text: binding(\.version))
                    TextField("Minimum iOS (\(info.minimum_os))", text: binding(\.minimum_os)).keyboardType(.decimalPad)
                    Toggle("Show in Files", isOn: $options.enable_file_sharing)
                    Toggle("Allow on All Devices", isOn: $options.force_all_devices)
                }
            }
        }
    }

    private func installedID(_ info: IpaInfo, team: String) -> String {
        let base = (options.bundle_id?.isEmpty == false ? options.bundle_id! : info.bundle_id)
        return base.hasSuffix(".\(team)") ? base : "\(base).\(team)"
    }

    private func restrictedCount(_ info: IpaInfo) -> Int? {
        mainBinary.map { $0.entitlements.object.keys.filter { !Entitlements.freeAllowed($0) }.count }
    }

    private func binding(_ kp: WritableKeyPath<InstallOptions, String?>) -> Binding<String> {
        Binding(get: { options[keyPath: kp] ?? "" }, set: { options[keyPath: kp] = $0.isEmpty ? nil : $0 })
    }

    /// Tweaks are kept in the data dir so refreshes can inject them again.
    private func addTweaks(_ urls: [URL]) {
        let dir = Core.dataDir.appendingPathComponent("tweaks", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let ext = url.pathExtension.lowercased()
            guard ext == "dylib" || ext == "framework" else {
                model.alert = AlertItem(title: "Not a tweak", message: "\(url.lastPathComponent) isn't a .dylib or .framework.")
                continue
            }
            let dest = dir.appendingPathComponent(url.lastPathComponent)
            try? FileManager.default.removeItem(at: dest)
            do {
                try FileManager.default.copyItem(at: url, to: dest)
                if !options.inject.contains(dest.path) { options.inject.append(dest.path) }
            } catch { model.show(error, title: "Couldn't add \(url.lastPathComponent)") }
        }
    }

    private func install() {
        guard let localIPA, let info else { return }
        model.install(ipa: localIPA, options: options, name: options.display_name ?? info.name, deleteAfter: true)  // the core keeps its own copy for refreshes
        if pending.temporary, localIPA != pending.ipa { try? FileManager.default.removeItem(at: pending.ipa) }
        dismiss()
    }

    private func cleanup() {
        if let localIPA, localIPA != pending.ipa || pending.temporary { try? FileManager.default.removeItem(at: localIPA) }
    }
}

struct LoadCommandsView: View {
    let commands: [String]
    @Binding var removed: [String]

    var body: some View {
        List {
            Section {
                ForEach(commands, id: \.self) { lc in
                    let system = lc.hasPrefix("/System/") || lc.hasPrefix("/usr/lib/")
                    Toggle(isOn: Binding(get: { !removed.contains(lc) }, set: { keep in
                        if keep { removed.removeAll { $0 == lc } } else { removed.append(lc) }
                    })) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(URL(fileURLWithPath: lc).lastPathComponent).font(.callout.weight(.medium))
                            Text(lc).font(.caption2.monospaced()).foregroundStyle(.secondary).lineLimit(2)
                        }
                    }
                    .disabled(system)
                }
            } footer: {
                Text("Turn off a library to stop the app from loading it, e.g. to remove a previously injected tweak. System libraries can't be removed.")
            }
        }
        .navigationTitle("Linked Libraries")
    }
}
