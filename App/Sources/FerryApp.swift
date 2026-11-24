import SwiftUI
import UniformTypeIdentifiers

extension UTType {
    static let ipa = UTType(importedAs: "com.apple.itunes.ipa", conformingTo: .zip)
    static let dylib = UTType(filenameExtension: "dylib") ?? .data
    static let framework = UTType(filenameExtension: "framework") ?? .package
}

struct FerryApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .task { await model.launch() }
        }
    }
}

/// What the install sheet is showing.
struct PendingInstall: Identifiable {
    let id = UUID()
    var ipa: URL
    var suggestedName: String
    /// From a source: what it declares, shown next to what the IPA asks for.
    var sourceApp: SourceApp?
    /// Downloaded/copied into our sandbox, so delete after installing.
    var temporary: Bool
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    @State private var tab = "library"
    @State private var pending: PendingInstall?

    var body: some View {
        @Bindable var model = model
        TabView(selection: $tab) {
            LibraryView(pending: $pending)
                .tabItem { Label("My Apps", systemImage: "square.stack.3d.up.fill") }.tag("library")
            BrowseView(pending: $pending)
                .tabItem { Label("Browse", systemImage: "sparkle.magnifyingglass") }.tag("browse")
            SourcesView()
                .tabItem { Label("Sources", systemImage: "tray.2.fill") }.tag("sources")
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape.fill") }.tag("settings")
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { OperationsBar() }
        .sheet(item: $pending) { p in InstallSheet(pending: p) }
        .sheet(item: $model.twoFactor) { p in TwoFactorSheet(prompt: p) }
        .alert(item: $model.revoke) { p in
            Alert(title: Text("Revoke a certificate?"),
                  message: Text("Your Apple ID already has the maximum number of development certificates:\n\n\(p.certificates.joined(separator: "\n"))\n\nRevoking one stops apps signed with it (on any device) from opening until they're refreshed."),
                  primaryButton: .destructive(Text("Revoke")) { model.answer(p.id, PromptAnswer(revoke: true)) },
                  secondaryButton: .cancel { model.answer(p.id, PromptAnswer(revoke: false)) })
        }
        .sheet(item: $model.pairingPin) { p in PairingPinSheet(prompt: p) }
        .alert(item: $model.alert) { a in Alert(title: Text(a.title), message: Text(a.message)) }
        .onOpenURL { handle($0) }
    }

    private func handle(_ url: URL) {
        if url.isFileURL, url.pathExtension.lowercased() == "ipa" {
            // Copy out of the inbox / provider so it survives.
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let dest = Downloads.dir.appendingPathComponent(url.lastPathComponent)
            try? FileManager.default.removeItem(at: dest)
            do {
                try FileManager.default.copyItem(at: url, to: dest)
                pending = PendingInstall(ipa: dest, suggestedName: url.deletingPathExtension().lastPathComponent, temporary: true)
            } catch { model.show(error, title: "Couldn't open the IPA") }
            return
        }
        // ferry://source?url=…, also AltStore/SideStore-style links.
        guard let c = URLComponents(url: url, resolvingAgainstBaseURL: false), c.host == "source",
              let s = c.queryItems?.first(where: { $0.name == "url" })?.value else { return }
        tab = "sources"
        Task {
            do { try await model.sources.add(s) } catch { model.show(error, title: "Couldn't add the source") }
        }
    }
}

// MARK: - Shared pieces

/// An app icon from a URL or a file (IPA icons are CgBI PNGs, which UIImage reads).
struct AppIcon: View {
    var url: URL?
    var size: CGFloat = 56

    var body: some View {
        Group {
            if let url, url.isFileURL, let img = UIImage(contentsOfFile: url.path) {
                Image(uiImage: img).resizable()
            } else if let url, !url.isFileURL {
                AsyncImage(url: url) { phase in
                    if let image = phase.image { image.resizable() } else { placeholder }
                }
            } else {
                placeholder
            }
        }
        .aspectRatio(contentMode: .fill)
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.225, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: size * 0.225, style: .continuous).strokeBorder(.quaternary, lineWidth: 0.5))
    }

    private var placeholder: some View {
        LinearGradient(colors: [.gray.opacity(0.35), .gray.opacity(0.2)], startPoint: .top, endPoint: .bottom)
            .overlay(Image(systemName: "app.dashed").font(.system(size: size * 0.4)).foregroundStyle(.secondary))
    }
}

/// "6 days" pill, red when nearly expired.
struct ExpiryBadge: View {
    var date: Date?

    var body: some View {
        if let date {
            let days = Calendar.current.dateComponents([.day], from: .now, to: date).day ?? 0
            let text = date < .now ? "Expired" : days < 1 ? "Today" : days == 1 ? "1 day" : "\(days) days"
            Text(text)
                .font(.caption.weight(.semibold).monospacedDigit())
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(Capsule().fill(color(days, expired: date < .now).opacity(0.18)))
                .foregroundStyle(color(days, expired: date < .now))
        }
    }

    private func color(_ days: Int, expired: Bool) -> Color {
        expired || days < 1 ? .red : days < 3 ? .orange : .green
    }
}

/// Running installs/refreshes, pinned above the tab bar.
struct OperationsBar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let downloads = model.downloads.active
        if !model.operations.isEmpty || !downloads.isEmpty {
            VStack(spacing: 8) {
                ForEach(downloads) { d in
                    row(title: "Downloading \(d.name)", stage: "\(Int(d.fraction * 100))%", fraction: d.fraction, cancel: nil)
                }
                ForEach(model.operations) { op in
                    row(title: op.title, stage: op.stage, fraction: op.fraction, cancel: op.cancel)
                }
            }
            .padding(12)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .padding(.horizontal, 12).padding(.bottom, 54)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .animation(.snappy, value: model.operations.count)
        }
    }

    private func row(title: String, stage: String, fraction: Double?, cancel: (() -> Void)?) -> some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.subheadline.weight(.semibold)).lineLimit(1)
                if let fraction {
                    ProgressView(value: min(max(fraction, 0), 1))
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
                Text(stage).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            if let cancel {
                Button(action: cancel) { Image(systemName: "xmark.circle.fill").font(.title3) }
                    .buttonStyle(.plain).foregroundStyle(.secondary)
                    .accessibilityLabel("Cancel")
            }
        }
    }
}

struct TwoFactorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let prompt: TwoFactorPrompt
    @State private var code = ""
    @State private var answered = false
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("123456", text: $code)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        .font(.system(.title, design: .rounded).monospacedDigit())
                        .multilineTextAlignment(.center)
                        .focused($focused)
                        .onChange(of: code) { if code.count == 6 { submit() } }
                } footer: {
                    Text(prompt.method == "sms"
                         ? "Enter the code sent to your phone."
                         : "Enter the code shown on your other Apple devices.")
                }
                if !prompt.phones.isEmpty {
                    Section("Didn't get a code?") {
                        ForEach(prompt.phones) { p in
                            Button("Text \(p.number)") { answer(PromptAnswer(sms: p.id)) }
                        }
                    }
                }
            }
            .navigationTitle("Two-Factor Authentication")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { answer(PromptAnswer(cancel: true)) } }
                ToolbarItem(placement: .confirmationAction) { Button("Verify", action: submit).disabled(code.count < 6) }
            }
            .onAppear { focused = true }
            .onDisappear { if !answered { model.answer(prompt.id, PromptAnswer(cancel: true)) } }
        }
        .presentationDetents([.medium, .large])
        .interactiveDismissDisabled()
    }

    private func submit() { answer(PromptAnswer(code: code)) }
    private func answer(_ a: PromptAnswer) {
        guard !answered else { return }
        answered = true
        model.answer(prompt.id, a)
        dismiss()
    }
}

/// The iPhone asked to pair again and shows a code in a system alert.
struct PairingPinSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let prompt: PairingPinPrompt
    @State private var code = ""
    @State private var answered = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("000000", text: $code)
                        .keyboardType(.numberPad).textContentType(.oneTimeCode)
                        .font(.system(.title, design: .rounded).monospacedDigit())
                        .multilineTextAlignment(.center)
                } footer: {
                    Text("This iPhone wants to pair with Ferry again and is showing a code. Enter it here.")
                }
            }
            .navigationTitle("Pair This iPhone")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { answer(PromptAnswer(cancel: true)) } }
                ToolbarItem(placement: .confirmationAction) { Button("Pair") { answer(PromptAnswer(code: code)) }.disabled(code.isEmpty) }
            }
            .onDisappear { if !answered { model.answer(prompt.id, PromptAnswer(cancel: true)) } }
        }
        .presentationDetents([.medium])
        .interactiveDismissDisabled()
    }

    private func answer(_ a: PromptAnswer) {
        guard !answered else { return }
        answered = true
        model.answer(prompt.id, a)
        dismiss()
    }
}

extension Int64 {
    var byteString: String { ByteCountFormatter.string(fromByteCount: self, countStyle: .file) }
}
