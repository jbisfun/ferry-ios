import SwiftUI

struct SourcesView: View {
    @Environment(AppModel.self) private var model
    @State private var adding = false
    @State private var newURL = ""
    @State private var busy = false

    var body: some View {
        NavigationStack {
            List {
                if model.sources.urls.isEmpty {
                    Section {
                        ForEach(SourceStore.suggestions, id: \.url) { s in
                            Button { add(s.url) } label: {
                                HStack {
                                    VStack(alignment: .leading) {
                                        Text(s.name).foregroundStyle(.primary)
                                        Text(s.url).font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Image(systemName: "plus.circle.fill").font(.title3)
                                }
                            }
                        }
                    } header: {
                        Text("Suggested")
                    } footer: {
                        Text("Sources are lists of apps published by their developers. GitHub repositories work too: Ferry lists the .ipa files attached to their releases.")
                    }
                }
                ForEach(model.sources.sources) { s in
                    NavigationLink(value: s) { SourceRow(source: s) }
                        .swipeActions { Button(role: .destructive) { model.sources.remove(s.url) } label: { Label("Remove", systemImage: "trash") } }
                }
                ForEach(model.sources.failures) { f in
                    HStack(spacing: 12) {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                        VStack(alignment: .leading) {
                            Text(f.url.host() ?? f.url.absoluteString).font(.headline)
                            Text(f.message).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .swipeActions { Button(role: .destructive) { model.sources.remove(f.url) } label: { Label("Remove", systemImage: "trash") } }
                }
            }
            .navigationTitle("Sources")
            .navigationDestination(for: Source.self) { SourceDetail(source: $0) }
            .refreshable { await model.sources.refresh() }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    if busy { ProgressView() } else {
                        Button { adding = true } label: { Image(systemName: "plus") }.accessibilityLabel("Add Source")
                    }
                }
            }
            .alert("Add Source", isPresented: $adding) {
                TextField("https://… or github.com/owner/repo", text: $newURL)
                    .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                Button("Add") { add(newURL); newURL = "" }
                Button("Cancel", role: .cancel) { newURL = "" }
            } message: {
                Text("An AltStore-compatible source URL, or a GitHub repository.")
            }
        }
    }

    private func add(_ url: String) {
        busy = true
        Task {
            do { try await model.sources.add(url) } catch { model.show(error, title: "Couldn't add the source") }
            busy = false
        }
    }
}

struct SourceRow: View {
    let source: Source

    var body: some View {
        HStack(spacing: 14) {
            AppIcon(url: source.iconURL, size: 44)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(source.name).font(.headline)
                    if source.kind == .github {
                        Text("GitHub").font(.caption2.weight(.semibold)).padding(.horizontal, 5).padding(.vertical, 1)
                            .background(Capsule().fill(.secondary.opacity(0.15)))
                    }
                }
                Text(source.subtitle ?? source.url.absoluteString).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer()
            Text("\(source.apps.count)").font(.subheadline.monospacedDigit()).foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }
}

struct SourceDetail: View {
    @Environment(AppModel.self) private var model
    let source: Source
    @State private var pending: PendingInstall?

    var body: some View {
        List {
            Section { Text(source.url.absoluteString).font(.caption.monospaced()).textSelection(.enabled) }
            Section("\(source.apps.count) apps") {
                ForEach(source.apps) { app in
                    NavigationLink { AppDetailView(app: app, pending: $pending) } label: { SourceAppRow(app: app, pending: $pending) }
                }
            }
        }
        .navigationTitle(source.name)
        .tint(source.tint)
        .sheet(item: $pending) { InstallSheet(pending: $0) }
    }
}
