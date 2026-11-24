import LynkeiKit
import SwiftUI
import UniformTypeIdentifiers

/// Traffic: toolbar, sources sidebar, filter chips, the call list and the inspector (canvas board 3).
struct TrafficScreen: View {
    @Environment(TrafficStore.self) private var store
    @Environment(Sheets.self) private var sheets
    @Binding var screen: Screen

    @AppStorage("trafficGrouped") private var grouped = false
    @State private var openGroups: Set<String> = []
    @State private var statusFilter = "all"
    @State private var methodFilter: String?
    @State private var typeFilter: String?
    @State private var versionFilter: String?
    @State private var deviceFilter: String?
    @State private var simulators: [BootedSimulator] = []
    @State private var simMessage: String?
    @AppStorage("sidebarWidth") private var sidebarWidth = 220.0
    @AppStorage("inspectorWidth") private var inspectorWidth = 440.0
    @State private var centerW = 900.0
    private static let centerMin = 520.0

    var body: some View {
        @Bindable var store = store
        VStack(spacing: 0) {
            AppHeader(screen: $screen) { toolbar(search: $store.search) }
            GeometryReader { geo in
                let sideMax = min(360, max(180, geo.size.width - inspectorWidth - Self.centerMin))
                let inspMax = min(760, max(320, geo.size.width - sidebarWidth - Self.centerMin))
                let side = min(max(sidebarWidth, 160), sideMax)
                let insp = min(max(inspectorWidth, 320), inspMax)
                let centerWidth = geo.size.width - side - insp
                HStack(spacing: 0) {
                    TrafficSidebar()
                        .frame(width: side)
                        .background(Theme.panel)
                        .overlay(alignment: .trailing) { Rectangle().fill(Theme.line).frame(width: 1) }
                        .overlay(alignment: .trailing) {
                            PaneDivider(width: $sidebarWidth, range: 160...sideMax, grows: .right)
                        }
                    center
                        .environment(\.trafficColumns, TrafficColumns(centerWidth: centerWidth))
                        .frame(width: max(centerWidth, 0), alignment: .leading)
                        .clipped()
                    InspectorView(tx: store.selected)
                        .frame(width: insp)
                        .background(Theme.panel)
                        .overlay(alignment: .leading) { Rectangle().fill(Theme.line).frame(width: 1) }
                        .overlay(alignment: .leading) {
                            PaneDivider(width: $inspectorWidth, range: 320...inspMax, grows: .left)
                        }
                }
                .onChange(of: centerWidth, initial: true) { _, w in centerW = w }
            }
        }
        .onAppear { simulators = Simulators.booted() }
    }

    // MARK: Toolbar

    private func toolbar(search: Binding<String>) -> some View {
        HStack(spacing: 8) {
            deviceMenu

            Button(action: { store.isRunning ? store.stop() : store.start() }) {
                HStack(spacing: 6) {
                    Circle().fill(store.isRunning ? Theme.s5xx : Theme.faint).frame(width: 8, height: 8)
                    Text(store.isRunning ? "Recording" : "Paused")
                }
                .font(.system(size: 12.5))
                .padding(.horizontal, 10)
                .frame(height: 30)
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
            }
            .buttonStyle(.plain)
            .fixedSize()
            .help(store.isRunning ? "Stop capturing" : "Start capturing")

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").font(.system(size: 12)).foregroundStyle(Theme.muted)
                TextField("status:5xx host:api duration:>800ms", text: search)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12, design: .monospaced))
                Text("⌘K")
                    .font(.system(size: 11)).foregroundStyle(Theme.faint)
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.line2))
            }
            .padding(.horizontal, 10)
            .frame(minWidth: 120, maxWidth: .infinity)
            .frame(height: 32)
            .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(store.searchError == nil ? Theme.line2 : Theme.s5xx))
            .help(store.searchError ?? "Filter language: app: client: host: path: method: status: header:name=value body: duration: size:")

            if scenarios.recordingSince != nil { RecordingBar().fixedSize() }

            toolsGroup

            FocusToggle()

            Button { store.clear() } label: {
                HStack(spacing: 5) { Image(systemName: "trash").font(.system(size: 11)); Text("Clear") }
                    .font(.system(size: 12.5, weight: .medium))
                    .padding(.horizontal, 10).frame(height: 30)
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
            }
            .buttonStyle(.plain).fixedSize()
            .help("Remove all captured calls from the list")

            moreMenu
        }
    }

    private var deviceMenu: some View {
        let ds = store.deviceStore()
        let live = ds.devices.filter { ds.isConnected($0) }
        return Menu {
            if ds.devices.isEmpty { Text("No simulators or phones found") }
            ForEach(ds.devices, id: \.id) { d in
                Text("\(ds.isConnected(d) ? "● " : "○ ")\(d.name) — \(d.os)")
            }
            Divider()
            Button("Connect device…") { sheets.showConnect = true }
            if !simulators.isEmpty { Divider() }
            ForEach(simulators) { sim in
                Button("Trust Lynkei CA in \(sim.name) (\(sim.runtime))") { installCA(sim) }
            }
            Button("Refresh") { simulators = Simulators.booted(); Task { await ds.refresh() } }
        } label: {
            HStack(spacing: 8) {
                Circle().fill(live.isEmpty ? Theme.faint : Theme.s2xx).frame(width: 7, height: 7)
                    .shadow(color: live.isEmpty ? .clear : Theme.s2xx.opacity(0.4), radius: 3)
                Text(live.isEmpty ? "No devices" : "\(live.count) device\(live.count == 1 ? "" : "s")").fontWeight(.medium)
                if !live.isEmpty {
                    Text(live.prefix(2).map(\.name).joined(separator: " · ")).foregroundStyle(Theme.muted).lineLimit(1)
                }
                Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold)).foregroundStyle(Theme.muted)
            }
            .font(.system(size: 12.5))
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
        }
        .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        .onAppear { ds.startWatching() }
    }

    /// SSL, Breakpoints, Rewrite, Map Local, Throttle: the live switches.
    private var toolsGroup: some View {
        HStack(spacing: 2) {
            toolToggle("SSL", on: store.decrypt, dot: Theme.s2xx, help: "Decrypt HTTPS so bodies can be read") { store.setDecrypt(!store.decrypt) }
            ForEach(["breakpoint", "rewrite", "map local"], id: \.self) { kind in
                let rules = store.rules.filter { $0.kind == kind }
                toolToggle(RuleStyle.group(kind), on: rules.contains(where: \.enabled), dot: kind == "breakpoint" ? Theme.paused : Theme.s2xx,
                           help: rules.isEmpty ? "No \(RuleStyle.group(kind)) rules yet" : "Switch all \(RuleStyle.group(kind)) rules on or off") {
                    let anyOn = rules.contains(where: \.enabled)
                    for r in rules { store.setRule(r.id, enabled: !anyOn) }
                }
            }
            Menu {
                Button("Off") { store.setGlobalThrottle(nil) }
                Divider()
                ForEach(store.throttlePresets, id: \.self) { p in
                    Button(p) { store.setGlobalThrottle(p) }
                }
            } label: {
                toolLabel("Slow network", on: store.globalThrottle != nil, dot: Theme.s4xx)
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.line))
        .fixedSize()
    }

    private func toolToggle(_ label: String, on: Bool, dot: Color, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { toolLabel(label, on: on, dot: dot) }
            .buttonStyle(.plain)
            .help(help)
    }

    private func toolLabel(_ label: String, on: Bool, dot: Color) -> some View {
        HStack(spacing: 6) {
            Circle().fill(on ? dot : Theme.line2).frame(width: 6, height: 6)
            Text(label)
        }
        .font(.system(size: 12, weight: .medium))
        .foregroundStyle(on ? Theme.fg : Theme.muted)
        .padding(.horizontal, 9)
        .frame(height: 26)
        .background(RoundedRectangle(cornerRadius: 6).fill(on ? Theme.panel : .clear))
        .contentShape(Rectangle())
    }

    private let scenarios = ScenarioStore.shared

    private var moreMenu: some View {
        Menu {
            Button("Compose request…") { sheets.composing = ComposeDraft() }
            Button(scenarios.recordingSince == nil ? "Record scenario" : "Stop recording scenario") {
                if scenarios.recordingSince == nil { scenarios.startRecording() } else { scenarios.cancelRecording() }
            }
            Divider()
            Button("Share session…") { SessionIO.share(store: store) }
            Button("Connect AI agent…") { sheets.showAgents = true }
            Button("Open session…") { SessionIO.open(store: store) }
            Divider()
            Toggle("Capture device logs", isOn: Binding(get: { LogStore.shared.enabled }, set: { LogStore.shared.enabled = $0 }))
            Divider()
            Button("Export HAR…") { exportHAR() }
            Button("Import HAR…") { importHAR() }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.muted)
                .frame(width: 30, height: 30)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
        }
        .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        .help("Compose a request, import or export HAR")
    }

    private func installCA(_ sim: BootedSimulator) {
        do {
            try Simulators.installCA(udid: sim.id, certPath: store.caPath)
            simMessage = "Lynkei CA trusted in \(sim.name)"
        } catch {
            simMessage = "Could not install CA: \(error.localizedDescription)"
        }
    }

    private func exportHAR() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "lynkei-traffic.har"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try store.exportHAR().write(to: url, atomically: true, encoding: .utf8)
        } catch {
            store.errorMessage = "Export failed: \(error.localizedDescription)"
        }
    }

    private func importHAR() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.json, .data]
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let n = try store.importHAR(String(contentsOf: url, encoding: .utf8))
            store.status = "Imported \(n) request\(n == 1 ? "" : "s") from \(url.lastPathComponent)"
        } catch {
            store.errorMessage = "Import failed: \(error)"
        }
    }

    // MARK: Center

    private var base: [FfiTransaction] { store.filtered }

    private func statusGroup(_ t: FfiTransaction) -> String {
        if t.state == .failed { return "failed" }
        switch t.status ?? 0 {
        case 200..<300: return "2xx"
        case 300..<400: return "3xx"
        case 400..<500: return "4xx"
        case 500...: return "5xx"
        default: return "other"
        }
    }

    private var rows: [FfiTransaction] {
        base.filter { t in
            if statusFilter == "failed", !(t.state == .failed || (t.status ?? 0) >= 500) { return false }
            if statusFilter != "all", statusFilter != "failed", statusGroup(t) != statusFilter { return false }
            if let m = methodFilter, t.method != m { return false }
            if let ty = typeFilter, Analytics.typeBadge(t) != ty { return false }
            if let v = versionFilter, Analytics.apiVersion(t.path) != v { return false }
            if let d = deviceFilter, Analytics.deviceLabel(t) != d { return false }
            return true
        }
    }

    private var center: some View {
        let shown = rows
        return VStack(spacing: 0) {
            chips
            TLSBanner(rows: base)
            tableHeader
            if store.transactions.isEmpty {
                EmptyStateView(port: store.port, caPath: store.caPath)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 0) {
                            if grouped {
                                ForEach(callGroups(shown)) { g in groupView(g) }
                            } else {
                                ForEach(shown) { tx in callRow(tx) }
                            }
                        }
                    }
                    .onChange(of: store.transactions.count) {
                        if !grouped, store.selection == nil, let last = shown.last { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
            footer(shown: shown.count)
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Grouping

    private struct CallGroup: Identifiable {
        let id: String
        var calls: [FfiTransaction]
    }

    /// Calls to the same endpoint (method, host and path with IDs folded), in order of first appearance.
    private func callGroups(_ rows: [FfiTransaction]) -> [CallGroup] {
        var order: [String] = []
        var map: [String: [FfiTransaction]] = [:]
        for t in rows {
            let k = Analytics.endpointKey(t)
            if map[k] == nil { order.append(k) }
            map[k, default: []].append(t)
        }
        return order.map { CallGroup(id: $0, calls: map[$0] ?? []) }
    }

    private func callRow(_ tx: FfiTransaction, indent: CGFloat = 0) -> some View {
        TrafficRow(tx: tx, selected: tx.id == store.selection, kinds: tx.appliedRules.map { store.ruleKind($0) })
            .padding(.leading, indent)
            .contentShape(Rectangle())
            .onTapGesture { store.selection = tx.id }
            .contextMenu { ruleMenu(for: tx) }
            .id(tx.id)
    }

    @ViewBuilder
    private func groupView(_ g: CallGroup) -> some View {
        if g.calls.count == 1, let only = g.calls.first {
            callRow(only)
        } else {
            let open = openGroups.contains(g.id)
            GroupHeader(calls: g.calls, open: open, kinds: g.calls.flatMap { $0.appliedRules.map { store.ruleKind($0) } })
                .contentShape(Rectangle())
                .onTapGesture { if open { openGroups.remove(g.id) } else { openGroups.insert(g.id) } }
            if open { ForEach(g.calls.reversed()) { callRow($0, indent: 18) } }
        }
    }

    private var viewSwitch: some View {
        PillGroup(items: [false, true], title: { $0 ? "Grouped" : "List" }, selection: $grouped, height: 24)
            .help("Group calls to the same endpoint, or list every call")
    }

    private var chips: some View {
        let list = base
        let counts: [String: Int] = [
            "all": list.count,
            "2xx": list.filter { statusGroup($0) == "2xx" }.count,
            "3xx": list.filter { statusGroup($0) == "3xx" }.count,
            "4xx": list.filter { statusGroup($0) == "4xx" }.count,
            "5xx": list.filter { statusGroup($0) == "5xx" }.count,
            "failed": list.filter { $0.state == .failed || ($0.status ?? 0) >= 500 }.count,
        ]
        let defs: [(String, String, Color)] = [
            ("all", "All", Theme.muted), ("2xx", "2xx", Theme.s2xx), ("3xx", "3xx", Theme.s3xx),
            ("4xx", "4xx", Theme.s4xx), ("5xx", "5xx", Theme.s5xx), ("failed", "Failed", Theme.failed),
        ]
        return FlowLayout(spacing: 6) {
            ForEach(defs, id: \.0) { id, label, dot in
                let on = statusFilter == id
                Button { statusFilter = id } label: {
                    HStack(spacing: 6) {
                        Circle().fill(dot).frame(width: 6, height: 6)
                        Text(label)
                        Text("\(counts[id] ?? 0)").font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted)
                    }
                    .font(.system(size: 12, weight: .medium))
                    .padding(.horizontal, 10)
                    .frame(height: 26)
                    .background(Capsule().fill(on ? Theme.selection : .clear))
                    .overlay(Capsule().stroke(on ? Theme.accent : Theme.line2))
                }
                .buttonStyle(.plain)
            }
            facet("Method", value: methodFilter, options: Array(Set(list.map(\.method))).sorted()) { methodFilter = $0 }
            facet("Type", value: typeFilter, options: Array(Set(list.map(Analytics.typeBadge))).filter { !$0.isEmpty }.sorted()) { typeFilter = $0 }
            facet("Device", value: deviceFilter, options: Array(Set(list.map(Analytics.deviceLabel))).sorted()) { deviceFilter = $0 }
            facet("API version", value: versionFilter, options: Array(Set(list.compactMap { Analytics.apiVersion($0.path) })).sorted()) { versionFilter = $0 }
            if let key = store.endpointFilter {
                Button { store.endpointFilter = nil } label: {
                    HStack(spacing: 5) {
                        Text(key).font(.system(size: 11.5, design: .monospaced)).lineLimit(1)
                        Image(systemName: "xmark").font(.system(size: 8, weight: .bold))
                    }
                    .padding(.horizontal, 9).frame(height: 26)
                    .background(Capsule().fill(Theme.selection))
                    .overlay(Capsule().stroke(Theme.accent))
                }
                .buttonStyle(.plain).help("Showing one endpoint. Click to show all calls.")
            }
            viewSwitch
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    private func facet(_ name: String, value: String?, options: [String], set: @escaping (String?) -> Void) -> some View {
        Menu {
            Button("Any") { set(nil) }
            if !options.isEmpty { Divider() }
            ForEach(options, id: \.self) { o in Button(o) { set(o) } }
        } label: {
            if let value {
                HStack(spacing: 5) {
                    Text("\(name): \(value)")
                    Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold))
                }
                .font(.system(size: 12)).foregroundStyle(Theme.fg)
                .padding(.horizontal, 9).frame(height: 26)
                .background(Capsule().fill(Theme.selection))
                .overlay(Capsule().stroke(Theme.accent))
            } else {
                DashedChip(label: name)
            }
        }
        .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
    }

    private var cols: TrafficColumns { TrafficColumns(centerWidth: centerW) }

    private var tableHeader: some View {
        HStack(spacing: 8) {
            Text("STATUS").frame(width: 52, alignment: .leading)
            Text("METHOD").frame(width: 58, alignment: .leading)
            Text("ENDPOINT").frame(minWidth: 150, maxWidth: .infinity, alignment: .leading)
            if cols.showHost { Text("HOST").frame(minWidth: 90, maxWidth: cols.hostMax, alignment: .leading) }
            if cols.showDevice { Text("DEVICE").frame(width: 70, alignment: .leading) }
            if cols.showDuration { Text("DURATION").frame(width: 104, alignment: .leading) }
            Text("SIZE").frame(width: 58, alignment: .trailing)
            Color.clear.frame(width: 26)
        }
        .font(.system(size: 11, weight: .semibold))
        .lineLimit(1)
        .tracking(0.4)
        .foregroundStyle(Theme.faint)
        .padding(.horizontal, 14)
        .frame(height: 30)
        .background(Theme.bg)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    @ViewBuilder
    private func ruleMenu(for tx: FfiTransaction) -> some View {
        Button("Modify this call…") { sheets.rewriting = RewriteDraft(copying: tx) }
        Divider()
        Button("Repeat") { store.repeatNow(tx) }
        Button("Edit & Repeat…") { sheets.composing = ComposeDraft(copying: tx) }
        Divider()
        Button("Pause calls to \(tx.host)\(tx.path.split(separator: "?").first.map { String($0) } ?? "")") { store.addQuickRule("breakpoint", for: tx) }
        Button("Block this request") { store.addQuickRule("block", for: tx) }
        Button("Mock with a file…") { sheets.mapLocalFor = tx }
        Button("Send \(tx.host) to another address…") { sheets.dnsFor = tx }
        Divider()
        Button("Slow to 3G") { store.addQuickRule("throttle-3g", for: tx) }
        Button("Make it fail (offline)") { store.addQuickRule("offline", for: tx) }
    }

    private func footer(shown: Int) -> some View {
        HStack(spacing: 16) {
            Text("**\(shown)** of \(store.transactions.count) requests")
                .foregroundStyle(Theme.muted)
            let failing = store.transactions.filter { $0.state == .failed || ($0.status ?? 0) >= 500 }.count
            if failing > 0 {
                Text("\(Text("●").foregroundStyle(Theme.s5xx)) \(failing) failing")
            }
            Text(formatBytes(store.transactions.reduce(0) { $0 + $1.responseBodySize + $1.requestBodySize }))
            if let simMessage { Text(simMessage).foregroundStyle(Theme.accent) }
            if let m = store.ruleMessage { Text(m).foregroundStyle(Theme.s5xx).lineLimit(1) }
            if let err = store.errorMessage { Text(err).foregroundStyle(Theme.s5xx).lineLimit(1) }
            Spacer()
            Text("\(store.status) · SSL \(store.decrypt ? "on" : "off")")
        }
        .font(.system(size: 11.5))
        .foregroundStyle(Theme.muted)
        .padding(.horizontal, 14)
        .frame(height: 28)
        .background(Theme.panel)
        .overlay(alignment: .top) { Rectangle().fill(Theme.line).frame(height: 1) }
    }
}

// MARK: - Row

struct TrafficRow: View {
    let tx: FfiTransaction
    let selected: Bool
    var kinds: [String] = []
    @Environment(\.trafficColumns) private var cols

    var body: some View {
        HStack(spacing: 8) {
            StatusPill(tx: tx).frame(width: 52, alignment: .leading)
            Text(tx.method)
                .font(.system(size: 11.5, weight: .semibold, design: .monospaced))
                .foregroundStyle(methodColor(tx.method))
                .frame(width: 58, alignment: .leading)
            HStack(spacing: 8) {
                let badge = Analytics.typeBadge(tx)
                if !badge.isEmpty {
                    Text(badge)
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.muted)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .overlay(RoundedRectangle(cornerRadius: 4).stroke(Theme.line2))
                }
                Text(rowTitle)
                    .font(.system(size: 12, design: .monospaced))
                    .lineLimit(1).truncationMode(.middle)
            }
            .frame(minWidth: 150, maxWidth: .infinity, alignment: .leading)
            if cols.showHost {
                Text(tx.host)
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                    .lineLimit(1).truncationMode(.middle)
                    .frame(minWidth: 90, maxWidth: cols.hostMax, alignment: .leading)
            }
            if cols.showDevice {
                HStack(spacing: 4) {
                    PlatformIcon(platform: Analytics.platform(ofDevice: Analytics.deviceLabel(tx)), size: 10)
                    Text(Analytics.deviceLabel(tx)).font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
                }
                .frame(width: 70, alignment: .leading)
            }
            if cols.showDuration { DurationBar(ms: tx.durationMs).frame(width: 104) }
            Text(tx.responseBodySize == 0 ? "—" : formatBytes(tx.responseBodySize))
                .font(.system(size: 11.5, design: .monospaced))
                .foregroundStyle(Theme.muted)
                .lineLimit(1)
                .frame(width: 58, alignment: .trailing)
            Group {
                if let kind = Array(Set(kinds.filter { !$0.isEmpty })).sorted().first {
                    RuleLetter(kind: kind).help("Rule applied: \(kind)")
                } else {
                    Color.clear
                }
            }
            .frame(width: 26)
        }
        .padding(.horizontal, 14)
        .frame(height: 32)
        .background(selected ? Theme.selection : (isError ? Theme.s5xx.opacity(0.05) : .clear))
        .overlay(alignment: .leading) { if selected { Rectangle().fill(Theme.accent).frame(width: 2) } }
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    private var isError: Bool { tx.state == .failed || (tx.status ?? 0) >= 500 }

    /// The path, plus what the call is when the path cannot say: the GraphQL operation or the gRPC method.
    private var rowTitle: String {
        if tx.state == .tunneled { return "\(tx.host):\(tx.port)" }
        if let rpc = GRPC.label(tx) { return rpc }
        if let op = GraphQL.label(tx) { return "\(tx.path.split(separator: "?").first.map(String.init) ?? tx.path) · \(op)" }
        return tx.path
    }
}

// MARK: - Sidebar

/// Devices, apps, domains with their endpoints, and rules (canvas board 3, left column).
struct TrafficSidebar: View {
    @Environment(TrafficStore.self) private var store
    @Environment(Sheets.self) private var sheets
    @State private var expanded: Set<String> = []
    @State private var seeded = false
    @State private var query = ""

    var body: some View {
        // Redo the counts once a second rather than on every captured call.
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            let hosts = hostSummary()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    devices
                    favourites
                    apps
                    domains(hosts)
                    rules
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 14)
            }
            .onAppear {
                if !seeded, let first = hosts.first { expanded = [first.host]; seeded = true }
            }
        }
    }

    private var devices: some View {
        let ds = store.deviceStore()
        return VStack(alignment: .leading, spacing: 2) {
            SectionLabel("Devices").padding(.leading, 8).padding(.bottom, 6)
            if ds.devices.isEmpty {
                Button { sheets.showConnect = true } label: {
                    Text("Connect a device…").font(.system(size: 12)).foregroundStyle(Theme.accent)
                        .padding(.horizontal, 8).padding(.vertical, 6)
                }
                .buttonStyle(.plain)
            }
            ForEach(ds.devices, id: \.id) { d in
                let on = ds.isConnected(d)
                Button { sheets.showConnect = true } label: {
                    HStack(spacing: 9) {
                        PlatformIcon(platform: d.kind.hasPrefix("ios") ? .ios : .android, size: 14, color: on ? Theme.fg : Theme.muted)
                        VStack(alignment: .leading, spacing: 0) {
                            Text(d.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                            Text(on ? "\(d.os) · ready" : "\(d.os) · not connected")
                                .font(.system(size: 11)).foregroundStyle(on ? Theme.muted : Theme.s4xx).lineLimit(1)
                        }
                        Spacer(minLength: 0)
                        Circle().fill(on ? Theme.s2xx : Theme.s4xx).frame(width: 7, height: 7)
                    }
                    .padding(.horizontal, 8).padding(.vertical, 6)
                    .background(RoundedRectangle(cornerRadius: 7).fill(on ? Theme.selection : .clear))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func appRow(_ name: String, count: Int?) -> some View {
        let on = store.appFilter == name
        return HStack(spacing: 9) {
            Text(String(name.prefix(1)).uppercased())
                .font(.system(size: 10, weight: .bold)).foregroundStyle(.white)
                .frame(width: 18, height: 18)
                .background(RoundedRectangle(cornerRadius: 5).fill(LinearGradient(colors: [Color(hex: 0xFF8A5B), Color(hex: 0xFF5277)], startPoint: .topLeading, endPoint: .bottomTrailing)))
            Text(name).font(.system(size: 13)).lineLimit(1)
            Spacer(minLength: 0)
            if let count { Text("\(count)").font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted) }
            StarButton(on: store.favApps.contains(name)) { store.toggleFavApp(name) }
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 7).fill(on ? Theme.selection : .clear))
        .contentShape(Rectangle())
        .onTapGesture { store.appFilter = on ? nil : name }
    }

    /// Favourite apps and endpoints, always on top.
    @ViewBuilder private var favourites: some View {
        if store.hasFavourites {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Image(systemName: "star.fill").font(.system(size: 9)).foregroundStyle(Theme.s4xx)
                    SectionLabel("Favourites")
                }
                .padding(.leading, 8).padding(.bottom, 6)
                ForEach(store.favApps.sorted(), id: \.self) { appRow($0, count: nil) }
                ForEach(store.favHosts.sorted(), id: \.self) { host in
                    HStack(spacing: 6) {
                        Image(systemName: "globe").font(.system(size: 10)).foregroundStyle(Theme.muted).frame(width: 14)
                        Text(host).font(.system(size: 12.5)).lineLimit(1)
                        Spacer(minLength: 0)
                        StarButton(on: true) { store.toggleFavHost(host) }
                    }
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(RoundedRectangle(cornerRadius: 7).fill(store.search == "host:\(host)" ? Theme.selection : .clear))
                    .contentShape(Rectangle())
                    .onTapGesture { store.showHost(host) }
                }
                ForEach(store.favEndpoints.sorted(), id: \.self) { key in
                    HStack(spacing: 6) {
                        let parts = key.split(separator: " ", maxSplits: 1).map(String.init)
                        Text(parts.first ?? "").font(.system(size: 10.5, weight: .semibold, design: .monospaced)).foregroundStyle(methodColor(parts.first ?? ""))
                        Text(parts.count > 1 ? parts[1] : key).font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.muted)
                            .lineLimit(1).truncationMode(.middle)
                        Spacer(minLength: 0)
                        StarButton(on: true) { store.toggleFavEndpoint(key) }
                    }
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(RoundedRectangle(cornerRadius: 7).fill(store.endpointFilter == key ? Theme.selection : .clear))
                    .contentShape(Rectangle())
                    .onTapGesture { store.showEndpoint(key) }
                }
            }
        }
    }

    private var apps: some View {
        let list = store.processesSeen.sorted { (store.favApps.contains($0.name) ? 0 : 1, -$0.count) < (store.favApps.contains($1.name) ? 0 : 1, -$1.count) }
        return VStack(alignment: .leading, spacing: 2) {
            SectionLabel("Apps").padding(.leading, 8).padding(.bottom, 6)
            if list.isEmpty {
                Text("Apps show up as they make requests").font(.system(size: 12)).foregroundStyle(Theme.faint).padding(.horizontal, 8)
            }
            ForEach(Array(list.prefix(8)), id: \.name) { appRow($0.name, count: $0.count) }
        }
    }

    private struct PathRow { let key: String; let method: String; let path: String; let color: Color }
    private struct HostRow { let host: String; let count: Int; let paths: [PathRow] }

    private func hostSummary() -> [HostRow] {
        var byHost: [String: [FfiTransaction]] = [:]
        for t in store.transactions { byHost[t.host, default: []].append(t) }
        let pausedHosts = Set(store.paused.compactMap { URL(string: $0.url)?.host })
        return byHost.map { host, txs in
            var byPath: [String: (n: Int, worst: Int, t: FfiTransaction)] = [:]
            for t in txs where t.state != .tunneled {
                let k = Analytics.endpointKey(t)
                var e = byPath[k] ?? (0, 0, t)
                e.n += 1
                let s = t.state == .failed ? 5 : Int(t.status ?? 0) / 100
                e.worst = max(e.worst, s >= 4 ? s : 2)
                byPath[k] = e
            }
            let paths = byPath.sorted { $0.value.n > $1.value.n }.map { k, e in
                PathRow(key: k, method: e.t.method, path: Analytics.foldPath(e.t.path),
                        color: e.worst >= 5 ? Theme.s5xx : e.worst == 4 ? Theme.s4xx : (pausedHosts.contains(host) ? Theme.paused : Theme.s2xx))
            }
            return HostRow(host: host, count: txs.count, paths: paths)
        }
        .sorted { $0.count > $1.count }
    }

    private func domains(_ all: [HostRow]) -> some View {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        let hosts = q.isEmpty ? all : all.filter { $0.host.lowercased().contains(q) || $0.paths.contains { $0.path.lowercased().contains(q) } }
        return VStack(alignment: .leading, spacing: 1) {
            SectionLabel("Domains").padding(.leading, 8).padding(.bottom, 6)
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").font(.system(size: 10)).foregroundStyle(Theme.muted)
                TextField("Find an endpoint", text: $query).textFieldStyle(.plain).font(.system(size: 12))
            }
            .padding(.horizontal, 8).frame(height: 26)
            .background(RoundedRectangle(cornerRadius: 7).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 7).stroke(Theme.line))
            .padding(.bottom, 4)
            if hosts.isEmpty {
                Text(all.isEmpty ? "None yet" : "No match").font(.system(size: 12)).foregroundStyle(Theme.faint).padding(.horizontal, 8)
            }
            ForEach(hosts.prefix(q.isEmpty ? 12 : 30), id: \.host) { h in
                let open = !q.isEmpty || expanded.contains(h.host)
                let active = store.search == "host:\(h.host)"
                HStack(spacing: 6) {
                    Image(systemName: open ? "chevron.down" : "chevron.right")
                        .font(.system(size: 8.5, weight: .bold)).foregroundStyle(Theme.muted).frame(width: 10)
                        .onTapGesture { if expanded.contains(h.host) { expanded.remove(h.host) } else { expanded.insert(h.host) } }
                    Text(h.host).font(.system(size: 13, weight: open ? .medium : .regular))
                        .foregroundStyle(open || active ? Theme.fg : Theme.muted).lineLimit(1)
                    Spacer(minLength: 0)
                    Text("\(h.count)").font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted)
                    StarButton(on: store.favHosts.contains(h.host)) { store.toggleFavHost(h.host) }
                }
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(RoundedRectangle(cornerRadius: 7).fill(active ? Theme.selection : .clear))
                .contentShape(Rectangle())
                .onTapGesture { if active { store.search = ""; store.selection = nil } else { store.showHost(h.host) } }
                if open {
                    let shown = h.paths.filter { q.isEmpty || h.host.lowercased().contains(q) || $0.path.lowercased().contains(q) }
                    ForEach(shown.prefix(q.isEmpty ? 6 : 20), id: \.key) { p in
                        HStack(spacing: 6) {
                            Circle().fill(p.color).frame(width: 5, height: 5)
                            Text(p.method).font(.system(size: 10, weight: .semibold, design: .monospaced)).foregroundStyle(methodColor(p.method))
                            Text(p.path).font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.muted).lineLimit(1)
                            Spacer(minLength: 0)
                            StarButton(on: store.favEndpoints.contains(p.key)) { store.toggleFavEndpoint(p.key) }
                        }
                        .padding(.leading, 22).padding(.trailing, 4).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 6).fill(store.endpointFilter == p.key ? Theme.selection : .clear))
                        .contentShape(Rectangle())
                        .onTapGesture { if store.endpointFilter == p.key { store.endpointFilter = nil } else { store.showEndpoint(p.key) } }
                    }
                }
            }
        }
    }

    private var rules: some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionLabel("Rules").padding(.leading, 8).padding(.bottom, 4)
            if store.rules.isEmpty {
                Text("Right-click a call to add one").font(.system(size: 12)).foregroundStyle(Theme.faint).padding(.horizontal, 8)
            }
            ForEach(store.rules.prefix(8), id: \.id) { r in
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 0) {
                        Text(RuleStyle.group(r.kind))
                            .font(.system(size: 12, weight: .medium))
                        Text(r.summary).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    LynkeiSwitch(isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                }
                .padding(.horizontal, 8).padding(.vertical, 5)
            }
        }
    }
}

// MARK: - Group header

/// One row standing for several calls to the same endpoint: how many, the worst outcome, the
/// average time and the total size. Click to open or close the calls inside.
struct GroupHeader: View {
    let calls: [FfiTransaction]
    let open: Bool
    var kinds: [String] = []

    private var rep: FfiTransaction {
        func rank(_ t: FfiTransaction) -> Int {
            if t.state == .failed { return 4 }
            let s = Int(t.status ?? 0) / 100
            return s >= 5 ? 3 : s == 4 ? 2 : 0
        }
        return calls.max { rank($0) < rank($1) || (rank($0) == rank($1) && $0.id < $1.id) } ?? calls[0]
    }

    var body: some View {
        let first = calls[0]
        let timed = calls.map(\.durationMs).filter { $0 > 0 }
        let avg = timed.isEmpty ? 0 : timed.reduce(0, +) / UInt64(timed.count)
        let failing = calls.filter { $0.state == .failed || ($0.status ?? 0) >= 400 }.count
        HStack(spacing: 8) {
            StatusPill(tx: rep).frame(width: 52, alignment: .leading)
            Text(first.method)
                .font(.system(size: 11.5, weight: .semibold, design: .monospaced))
                .foregroundStyle(methodColor(first.method)).frame(width: 58, alignment: .leading)
            HStack(spacing: 8) {
                Image(systemName: open ? "chevron.down" : "chevron.right")
                    .font(.system(size: 9, weight: .bold)).foregroundStyle(Theme.muted).frame(width: 10)
                Text("×\(calls.count)")
                    .font(.system(size: 10.5, weight: .bold, design: .monospaced))
                    .foregroundStyle(Theme.accent)
                    .padding(.horizontal, 6).padding(.vertical, 1)
                    .background(Capsule().fill(Theme.selection))
                Text(first.state == .tunneled ? "\(first.host):\(first.port)" : Analytics.foldPath(first.path))
                    .font(.system(size: 12, weight: .medium, design: .monospaced))
                    .lineLimit(1).truncationMode(.middle)
                if failing > 0 {
                    Text("\(failing) failed").font(.system(size: 10.5)).foregroundStyle(Theme.s5xx).lineLimit(1)
                }
            }
            .frame(minWidth: 150, maxWidth: .infinity, alignment: .leading)
            Text(first.host).font(.system(size: 12)).foregroundStyle(Theme.muted)
                .lineLimit(1).truncationMode(.middle).frame(minWidth: 90, maxWidth: 150, alignment: .leading)
            HStack(spacing: 4) {
                PlatformIcon(platform: Analytics.platform(ofDevice: Analytics.deviceLabel(first)), size: 10)
                Text(Analytics.deviceLabel(first)).font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
            }
            .frame(width: 70, alignment: .leading)
            DurationBar(ms: avg).frame(width: 104)
            let total = calls.reduce(0) { $0 + $1.responseBodySize }
            Text(total == 0 ? "—" : formatBytes(total))
                .font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.muted).lineLimit(1)
                .frame(width: 58, alignment: .trailing)
            Group {
                if let kind = Array(Set(kinds.filter { !$0.isEmpty })).sorted().first { RuleLetter(kind: kind) } else { Color.clear }
            }
            .frame(width: 26)
        }
        .padding(.horizontal, 14)
        .frame(height: 32)
        .background(Theme.raised.opacity(0.5))
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }
}


// MARK: - Resizable panes

/// Which optional columns the call list has room for; the endpoint column takes whatever is left.
struct TrafficColumns {
    var centerWidth: Double = 900
    var showHost: Bool { centerWidth >= 600 }
    var showDevice: Bool { centerWidth >= 760 }
    var showDuration: Bool { centerWidth >= 900 }
    /// Hosts get more room as the list widens.
    var hostMax: CGFloat { centerWidth >= 1100 ? 240 : 150 }
}

private struct TrafficColumnsKey: EnvironmentKey { static let defaultValue = TrafficColumns() }
extension EnvironmentValues {
    var trafficColumns: TrafficColumns {
        get { self[TrafficColumnsKey.self] }
        set { self[TrafficColumnsKey.self] = newValue }
    }
}

/// A thin drag handle on a pane's edge. Drag to resize within `range`; double-click to reset.
struct PaneDivider: View {
    enum Grows { case left, right }
    @Binding var width: Double
    let range: ClosedRange<Double>
    let grows: Grows
    @State private var start: Double?
    @State private var hover = false

    var body: some View {
        Rectangle().fill(hover || start != nil ? Theme.accent.opacity(0.5) : .clear)
            .frame(width: 3)
            .contentShape(Rectangle().inset(by: -3))
            .offset(x: grows == .right ? 1 : -1)
            .onHover { inside in
                hover = inside
                if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
            }
            .gesture(
                DragGesture(minimumDistance: 1, coordinateSpace: .global)
                    .onChanged { v in
                        let base = start ?? width
                        start = base
                        let delta = grows == .right ? v.translation.width : -v.translation.width
                        width = min(max(base + delta, range.lowerBound), range.upperBound)
                    }
                    .onEnded { _ in start = nil }
            )
            .onTapGesture(count: 2) { width = grows == .right ? 220 : 440 }
    }
}
