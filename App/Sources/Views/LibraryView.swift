import SwiftUI
import UniformTypeIdentifiers

struct LibraryView: View {
    @Environment(AppModel.self) private var model
    @Binding var pending: PendingInstall?
    @State private var importing = false
    @State private var removing: LibraryApp?

    private var ferryInstalled: Set<String> { Set(model.library.map(\.installed_bundle_id)) }
    private var otherApps: [InstalledApp] {
        model.deviceApps.filter { $0.sideloaded && !ferryInstalled.contains($0.bundle_id) && $0.bundle_id != Bundle.main.bundleIdentifier }
    }

    var body: some View {
        NavigationStack {
            List {
                if case .failed(let message) = model.device {
                    DeviceBanner(message: message)
                }
                if model.library.isEmpty {
                    ContentUnavailableView {
                        Label("No Apps Yet", systemImage: "shippingbox")
                    } description: {
                        Text("Install an app from Browse, or import an .ipa from Files.")
                    } actions: {
                        Button("Import IPA") { importing = true }.buttonStyle(.borderedProminent)
                    }
                    .listRowBackground(Color.clear)
                } else {
                    Section {
                        ForEach(model.library) { app in
                            NavigationLink(value: app) { LibraryRow(app: app) }
                                .swipeActions(edge: .leading) {
                                    Button { model.refresh([app]) } label: { Label("Refresh", systemImage: "arrow.clockwise") }.tint(.blue)
                                }
                                .swipeActions(edge: .trailing) {
                                    Button(role: .destructive) { removing = app } label: { Label("Remove", systemImage: "trash") }
                                }
                                .contextMenu {
                                    Button { model.refresh([app]) } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                                    Button(role: .destructive) { removing = app } label: { Label("Remove", systemImage: "trash") }
                                }
                        }
                    } header: {
                        Text("Installed by Ferry")
                    } footer: {
                        if model.account?.free == true {
                            Text("Free Apple IDs can run 3 sideloaded apps at a time, and each must be refreshed every 7 days.")
                        }
                    }
                }
                if !otherApps.isEmpty {
                    Section {
                        ForEach(otherApps) { app in
                            HStack {
                                VStack(alignment: .leading) {
                                    Text(app.name).font(.body.weight(.medium))
                                    Text(app.bundle_id).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Text(app.version).font(.caption).foregroundStyle(.secondary)
                            }
                            .swipeActions {
                                Button(role: .destructive) { model.uninstall(app) } label: { Label("Delete", systemImage: "trash") }
                            }
                        }
                    } header: {
                        Text("Other sideloaded apps")
                    } footer: {
                        Text("Installed by another tool. Ferry can't refresh these, but they count toward the free-account limit.")
                    }
                }
            }
            .navigationTitle("My Apps")
            .navigationDestination(for: LibraryApp.self) { LibraryDetailView(app: $0) }
            .refreshable { await model.reloadLibrary(); await model.checkDevice() }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Refresh All") { model.refresh() }.disabled(model.library.isEmpty)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { importing = true } label: { Image(systemName: "plus") }.accessibilityLabel("Import IPA")
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.ipa, .zip]) { result in
                guard case .success(let url) = result else { return }
                pending = PendingInstall(ipa: url, suggestedName: url.deletingPathExtension().lastPathComponent, temporary: false)
            }
            .confirmationDialog("Remove \(removing?.name ?? "")?", isPresented: .init(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                                titleVisibility: .visible, presenting: removing) { app in
                Button("Delete App", role: .destructive) { model.remove(app, uninstall: true) }
                Button("Stop Refreshing Only") { model.remove(app, uninstall: false) }
            } message: { _ in
                Text("Deleting removes the app and its data from this iPhone.")
            }
        }
    }
}

struct LibraryRow: View {
    let app: LibraryApp

    var body: some View {
        HStack(spacing: 14) {
            AppIcon(url: app.icon_path.map(URL.init(fileURLWithPath:)), size: 52)
            VStack(alignment: .leading, spacing: 3) {
                Text(app.name).font(.headline).lineLimit(1)
                Text("Version \(app.version)").font(.subheadline).foregroundStyle(.secondary)
                if !app.options.inject.isEmpty {
                    Label("\(app.options.inject.count) injected", systemImage: "syringe")
                        .font(.caption).foregroundStyle(.purple)
                }
            }
            Spacer()
            ExpiryBadge(date: app.expiryDate)
        }
        .padding(.vertical, 4)
    }
}

struct DeviceBanner: View {
    @Environment(AppModel.self) private var model
    var message: String

    var body: some View {
        Section {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "network.slash").font(.title2).foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Not connected to this iPhone").font(.headline)
                    Text(message).font(.subheadline).foregroundStyle(.secondary)
                    Button("Try Again") { Task { await model.checkDevice() } }.font(.subheadline.weight(.semibold))
                }
            }
            .padding(.vertical, 4)
        }
    }
}

struct LibraryDetailView: View {
    @Environment(AppModel.self) private var model
    let app: LibraryApp

    var body: some View {
        List {
            Section {
                HStack(spacing: 16) {
                    AppIcon(url: app.icon_path.map(URL.init(fileURLWithPath:)), size: 72)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(app.name).font(.title2.bold())
                        Text(app.installed_bundle_id).font(.caption.monospaced()).foregroundStyle(.secondary)
                        ExpiryBadge(date: app.expiryDate)
                    }
                }
                .listRowBackground(Color.clear)
            }
            Section {
                Button { model.refresh([app]) } label: { Label("Refresh Now", systemImage: "arrow.clockwise") }
            }
            Section("Details") {
                LabeledContent("Version", value: app.version)
                LabeledContent("Original bundle ID", value: app.original_bundle_id)
                LabeledContent("Team", value: app.team_id)
                if let d = parseISODate(app.installed_at) { LabeledContent("Installed", value: d.formatted(date: .abbreviated, time: .shortened)) }
                if let e = app.expiryDate { LabeledContent("Expires", value: e.formatted(date: .abbreviated, time: .shortened)) }
            }
            if !app.options.inject.isEmpty {
                Section("Injected") {
                    ForEach(app.options.inject, id: \.self) { Text(URL(fileURLWithPath: $0).lastPathComponent).font(.callout.monospaced()) }
                }
            }
            Section {
                NavigationLink {
                    EntitlementsList(title: "Granted Entitlements", entitlements: app.granted_entitlements.object)
                } label: {
                    LabeledContent("Granted Entitlements", value: "\(app.granted_entitlements.object.count)")
                }
            } footer: {
                Text("What the provisioning profile actually allowed, read from the signed app.")
            }
        }
        .navigationTitle(app.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
