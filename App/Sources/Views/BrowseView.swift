import SwiftUI

struct BrowseView: View {
    @Environment(AppModel.self) private var model
    @Binding var pending: PendingInstall?
    @State private var query = ""

    private var apps: [SourceApp] {
        let all = model.sources.allApps
        guard !query.isEmpty else { return all }
        return all.filter {
            $0.name.localizedCaseInsensitiveContains(query) || $0.developer.localizedCaseInsensitiveContains(query)
                || ($0.subtitle ?? "").localizedCaseInsensitiveContains(query)
        }
    }

    var body: some View {
        NavigationStack {
            Group {
                if model.sources.urls.isEmpty {
                    ContentUnavailableView("No Sources", systemImage: "tray",
                                           description: Text("Add an AltStore source or a GitHub repository in the Sources tab."))
                } else if apps.isEmpty && model.sources.loading {
                    ProgressView("Loading sources…")
                } else if apps.isEmpty {
                    ContentUnavailableView.search(text: query)
                } else {
                    List(apps) { app in
                        NavigationLink(value: app) { SourceAppRow(app: app, pending: $pending) }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle("Browse")
            .navigationDestination(for: SourceApp.self) { AppDetailView(app: $0, pending: $pending) }
            .searchable(text: $query, prompt: "Apps, developers")
            .refreshable { await model.sources.refresh() }
        }
    }
}

struct SourceAppRow: View {
    let app: SourceApp
    @Binding var pending: PendingInstall?

    var body: some View {
        HStack(spacing: 14) {
            AppIcon(url: app.iconURL, size: 56)
            VStack(alignment: .leading, spacing: 2) {
                Text(app.name).font(.headline).lineLimit(1)
                Text(app.subtitle ?? app.developer).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                Text(app.sourceName).font(.caption2).foregroundStyle(.tertiary)
            }
            Spacer(minLength: 8)
            GetButton(app: app, pending: $pending)
        }
        .padding(.vertical, 4)
    }
}

/// App Store-style "GET" capsule: downloads, then opens the install sheet.
struct GetButton: View {
    @Environment(AppModel.self) private var model
    let app: SourceApp
    var version: SourceAppVersion?
    @Binding var pending: PendingInstall?
    @State private var busy = false

    private var installed: Bool {
        model.library.contains { $0.original_bundle_id == app.bundleID }
    }

    var body: some View {
        Button {
            busy = true
            Task {
                if let file = await model.download(app, version: version) {
                    pending = PendingInstall(ipa: file, suggestedName: app.name, sourceApp: app, temporary: true)
                }
                busy = false
            }
        } label: {
            Group {
                if busy { ProgressView().controlSize(.small) } else { Text(installed ? "UPDATE" : "GET") }
            }
            .font(.subheadline.weight(.bold))
            .frame(minWidth: 52)
            .padding(.vertical, 6).padding(.horizontal, 10)
            .background(Capsule().fill((app.tint ?? .accentColor).opacity(0.15)))
            .foregroundStyle(app.tint ?? .accentColor)
        }
        .buttonStyle(.plain)
        .disabled(busy || (version ?? app.latest) == nil)
    }
}

struct AppDetailView: View {
    let app: SourceApp
    @Binding var pending: PendingInstall?
    @State private var showAllVersions = false

    var body: some View {
        List {
            Section {
                HStack(alignment: .top, spacing: 16) {
                    AppIcon(url: app.iconURL, size: 96)
                    VStack(alignment: .leading, spacing: 6) {
                        Text(app.name).font(.title2.bold())
                        Text(app.developer).foregroundStyle(.secondary)
                        Spacer(minLength: 4)
                        GetButton(app: app, pending: $pending)
                    }
                }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets(top: 8, leading: 0, bottom: 8, trailing: 0))
            }

            if !app.screenshots.isEmpty {
                Section {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 10) {
                            ForEach(app.screenshots, id: \.self) { url in
                                AsyncImage(url: url) { $0.resizable().scaledToFit() } placeholder: { Color.gray.opacity(0.15) }
                                    .frame(height: 380)
                                    .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                            }
                        }
                    }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                }
            }

            if let d = app.description, !d.isEmpty {
                Section("About") { Text(d).font(.callout) }
            }

            if let v = app.latest {
                Section("What's New") {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text("Version \(v.version)").font(.subheadline.weight(.semibold))
                            Spacer()
                            if let date = v.date { Text(date, style: .date).font(.caption).foregroundStyle(.secondary) }
                        }
                        if let n = v.notes, !n.isEmpty { Text(n).font(.callout).foregroundStyle(.secondary).lineLimit(8) }
                    }
                    if app.versions.count > 1 {
                        NavigationLink("Version History") { VersionHistory(app: app, pending: $pending) }
                    }
                }
            }

            if !app.entitlements.isEmpty || !app.privacy.isEmpty {
                Section {
                    ForEach(app.entitlements) { p in
                        EntitlementRow(key: p.name, value: nil)
                    }
                    ForEach(app.privacy) { p in
                        VStack(alignment: .leading, spacing: 2) {
                            Label(Entitlements.privacyName(p.name), systemImage: Entitlements.privacyIcon(p.name)).font(.subheadline.weight(.medium))
                            if let u = p.usage { Text(u).font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                } header: {
                    Text("Permissions")
                } footer: {
                    Text("Declared by the source. The install screen shows what the app actually asks for.")
                }
            }

            Section("Information") {
                LabeledContent("Source", value: app.sourceName)
                LabeledContent("Bundle ID", value: app.bundleID)
                if let s = app.latest?.size { LabeledContent("Size", value: s.byteString) }
                if let m = app.latest?.minOS { LabeledContent("Requires", value: "iOS \(m) or later") }
                if let c = app.category { LabeledContent("Category", value: c.capitalized) }
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .tint(app.tint)
    }
}

struct VersionHistory: View {
    let app: SourceApp
    @Binding var pending: PendingInstall?

    var body: some View {
        List(app.versions) { v in
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(v.version).font(.headline)
                    if let d = v.date { Text(d, style: .date).font(.caption).foregroundStyle(.secondary) }
                    if let n = v.notes, !n.isEmpty { Text(n).font(.caption).foregroundStyle(.secondary).lineLimit(4) }
                }
                Spacer()
                GetButton(app: app, version: v, pending: $pending)
            }
        }
        .navigationTitle("Version History")
    }
}
