import RelayKit
import SwiftUI

struct ContentView: View {
    @Environment(TrafficStore.self) private var store
    @State private var simulators: [BootedSimulator] = []
    @State private var simMessage: String?
    @State private var showConnect = false
    @State private var showRules = false

    var body: some View {
        @Bindable var store = store
        VStack(spacing: 0) {
            toolbar(search: $store.search)
            Divider().overlay(Theme.line)
            HStack(spacing: 0) {
                trafficList
                Divider().overlay(Theme.line)
                InspectorView(tx: store.selected)
                    .frame(width: 440)
                    .background(Theme.panel)
            }
            Divider().overlay(Theme.line)
            footer
        }
        .background(Theme.bg)
        .foregroundStyle(Theme.fg)
        .onAppear { simulators = Simulators.booted() }
        .sheet(isPresented: $showConnect) { ConnectView() }
        .sheet(isPresented: $showRules) { RulesView() }
        .sheet(item: Binding(get: { store.paused.first }, set: { _ in })) { BreakpointView(item: $0).id($0.id) }
    }

    // MARK: Toolbar

    private func toolbar(search: Binding<String>) -> some View {
        HStack(spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "arrow.triangle.swap").foregroundStyle(Theme.accent)
                Text("Relay").font(.system(size: 14, weight: .semibold))
            }
            .padding(.leading, 72)  // clear the traffic-light buttons

            Button(action: { store.isRunning ? store.stop() : store.start() }) {
                HStack(spacing: 6) {
                    Circle().fill(store.isRunning ? Theme.s5xx : Theme.faint).frame(width: 8, height: 8)
                    Text(store.isRunning ? "Recording" : "Start")
                }
            }
            .buttonStyle(PillButtonStyle())

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                TextField("Search: app:AcmeShop host:api status:5xx header:user-agent=…", text: search)
                    .textFieldStyle(.plain)
                    .font(Theme.mono)
            }
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.line2))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(store.searchError == nil ? Color.clear : Theme.s5xx))
            .help(store.searchError ?? "Filter language: app: client: host: path: method: status: header:name=value body: duration: size:")
            .frame(maxWidth: 520)

            sourceMenu

            Button(action: { showRules = true }) {
                HStack(spacing: 6) {
                    Image(systemName: "slider.horizontal.3")
                    Text("Rules")
                    if store.rules.contains(where: \.enabled) {
                        Text("\(store.rules.filter(\.enabled).count)").foregroundStyle(Theme.accent)
                    }
                }
            }
            .buttonStyle(PillButtonStyle())

            Spacer()

            Menu {
                if simulators.isEmpty {
                    Text("No booted simulators — boot one in Xcode")
                }
                ForEach(simulators) { sim in
                    Button("Trust Relay CA in \(sim.name) (\(sim.runtime))") { installCA(sim) }
                }
                Divider()
                Button("Refresh simulators") { simulators = Simulators.booted() }
            } label: {
                Label("Simulators", systemImage: "iphone")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()

            Button(action: { showConnect = true }) {
                Label("Connect device", systemImage: "plus.circle.fill")
            }
            .buttonStyle(PillButtonStyle())

            Button("Clear") { store.clear() }.buttonStyle(PillButtonStyle())
        }
        .padding(.horizontal, 14)
        .frame(height: 52)
        .background(Theme.panel)
    }

    private var sourceMenu: some View {
        Menu {
            Button("All traffic") { store.source = nil }
            let procs = store.processesSeen
            if !procs.isEmpty { Divider() }
            ForEach(procs, id: \.name) { p in
                Button("\(p.name)  (\(p.count))") { store.source = "app:\(p.name.contains(" ") ? "\"\(p.name)\"" : p.name)" }
            }
        } label: {
            HStack(spacing: 5) {
                Image(systemName: "app.dashed")
                Text(store.source.map { $0.replacingOccurrences(of: "app:", with: "") } ?? "All apps")
                    .lineLimit(1)
            }
            .foregroundStyle(store.source == nil ? Theme.muted : Theme.accent)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Only show requests from one program on this Mac (the iOS Simulator app, a browser, …)")
    }

    private func installCA(_ sim: BootedSimulator) {
        do {
            try Simulators.installCA(udid: sim.id, certPath: store.caPath)
            simMessage = "Relay CA trusted in \(sim.name)"
        } catch {
            simMessage = "Could not install CA: \(error.localizedDescription)"
        }
    }

    // MARK: List

    private var trafficList: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text("STATUS").frame(width: 58, alignment: .leading)
                Text("METHOD").frame(width: 64, alignment: .leading)
                Text("URL").frame(maxWidth: .infinity, alignment: .leading)
                Text("APP").frame(width: 110, alignment: .leading)
                Text("DURATION").frame(width: 120, alignment: .leading)
                Text("SIZE").frame(width: 70, alignment: .trailing)
            }
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Theme.faint)
            .padding(.horizontal, 14)
            .frame(height: 30)
            Divider().overlay(Theme.line)

            if store.transactions.isEmpty {
                EmptyStateView(port: store.port, caPath: store.caPath)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 0) {
                            ForEach(store.filtered) { tx in
                                TrafficRow(tx: tx, selected: tx.id == store.selection, kinds: tx.appliedRules.map { store.ruleKind($0) })
                                    .contentShape(Rectangle())
                                    .onTapGesture { store.selection = tx.id }
                                    .contextMenu { ruleMenu(for: tx) }
                                    .id(tx.id)
                            }
                        }
                    }
                    .onChange(of: store.transactions.count) {
                        if store.selection == nil, let last = store.filtered.last { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder
    private func ruleMenu(for tx: FfiTransaction) -> some View {
        Button("Breakpoint on \(tx.host)\(tx.path.split(separator: "?").first.map { String($0) } ?? "")") { store.addQuickRule("breakpoint", for: tx) }
        Button("Block this request") { store.addQuickRule("block", for: tx) }
        Button("Map local: answer with this response") { store.addQuickRule("map-local", for: tx) }
            .disabled(tx.responseBodyPreview == nil)
        Divider()
        Button("Throttle to 3G") { store.addQuickRule("throttle-3g", for: tx) }
        Button("Make it fail (offline)") { store.addQuickRule("offline", for: tx) }
    }

    private var footer: some View {
        HStack(spacing: 16) {
            Text("\(store.filtered.count) of \(store.transactions.count) requests")
            Text("\(store.transactions.filter { $0.state == .failed || ($0.status ?? 0) >= 500 }.count) failing")
                .foregroundStyle(Theme.s5xx)
            if let simMessage { Text(simMessage).foregroundStyle(Theme.accent) }
            if let m = store.ruleMessage { Text(m).foregroundStyle(Theme.s5xx).lineLimit(1) }
            if let err = store.errorMessage { Text(err).foregroundStyle(Theme.s5xx).lineLimit(1) }
            Spacer()
            Text(store.status)
        }
        .font(.system(size: 11.5))
        .foregroundStyle(Theme.muted)
        .padding(.horizontal, 14)
        .frame(height: 28)
        .background(Theme.panel)
    }
}

// MARK: - Rows

struct TrafficRow: View {
    let tx: FfiTransaction
    let selected: Bool
    var kinds: [String] = []

    var body: some View {
        HStack(spacing: 10) {
            StatusPill(tx: tx).frame(width: 58, alignment: .leading)
            Text(tx.method)
                .font(Theme.monoSmall)
                .foregroundStyle(tx.method == "GET" ? Theme.muted : Theme.accent)
                .frame(width: 64, alignment: .leading)
            HStack(spacing: 6) {
                if tx.decrypted {
                    Image(systemName: "lock.open.fill").font(.system(size: 9)).foregroundStyle(Theme.s2xx)
                } else if tx.scheme == "https" {
                    Image(systemName: "lock.fill").font(.system(size: 9)).foregroundStyle(Theme.faint)
                }
                Text(tx.host).foregroundStyle(Theme.muted)
                Text(tx.path).foregroundStyle(Theme.fg)
                ForEach(Array(Set(kinds.filter { !$0.isEmpty })).sorted(), id: \.self) { k in
                    Image(systemName: RuleBadge.symbol(k)).font(.system(size: 10)).foregroundStyle(Theme.paused)
                        .help("Rule applied: \(k)")
                }
            }
            .font(Theme.mono)
            .lineLimit(1)
            .truncationMode(.middle)
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(tx.clientProcess ?? "")
                .font(Theme.monoSmall)
                .foregroundStyle(Theme.muted)
                .lineLimit(1)
                .frame(width: 110, alignment: .leading)
            DurationBar(ms: tx.durationMs).frame(width: 120)
            Text(formatBytes(tx.responseBodySize))
                .font(Theme.mono)
                .foregroundStyle(Theme.muted)
                .frame(width: 70, alignment: .trailing)
        }
        .padding(.horizontal, 14)
        .frame(height: 30)
        .background(selected ? Theme.selection : (isError ? Theme.s5xx.opacity(0.05) : .clear))
        .overlay(alignment: .leading) { if selected { Rectangle().fill(Theme.accent).frame(width: 2) } }
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    private var isError: Bool { tx.state == .failed || (tx.status ?? 0) >= 500 }
}

struct StatusPill: View {
    let tx: FfiTransaction
    var body: some View {
        Text(label)
            .font(Theme.monoSmall)
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 5).fill(color.opacity(0.14)))
    }
    private var label: String {
        switch tx.state {
        case .tunneled: return "TUN"
        case .failed: return "ERR"
        case .inProgress: return tx.status.map { "\($0)…" } ?? "…"
        case .complete: return tx.status.map(String.init) ?? "—"
        }
    }
    private var color: Color { statusColor(tx) }
}

func statusColor(_ tx: FfiTransaction) -> Color {
    if tx.state == .failed { return Theme.failed }
    if tx.state == .tunneled { return Theme.s3xx }
    if tx.state == .inProgress { return Theme.accent }
    switch tx.status ?? 0 {
    case 200..<300: return Theme.s2xx
    case 300..<400: return Theme.s3xx
    case 400..<500: return Theme.s4xx
    default: return Theme.s5xx
    }
}

struct DurationBar: View {
    let ms: UInt64
    var body: some View {
        HStack(spacing: 8) {
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Theme.line)
                    Capsule().fill(ms > 800 ? Theme.s4xx : Theme.accent)
                        .frame(width: max(2, geo.size.width * min(1, Double(ms) / 2000)))
                }
            }
            .frame(height: 4)
            Text(formatMs(ms))
                .font(Theme.mono)
                .foregroundStyle(ms > 800 ? Theme.s4xx : Theme.fg)
                .frame(width: 54, alignment: .trailing)
        }
    }
}

struct PillButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12, weight: .medium))
            .padding(.horizontal, 11)
            .frame(height: 28)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised.opacity(configuration.isPressed ? 0.6 : 1)))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
    }
}

struct EmptyStateView: View {
    let port: UInt16?
    let caPath: String
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Waiting for traffic").font(.system(size: 18, weight: .semibold))
            Text("Point a client at this proxy to see requests here.").foregroundStyle(Theme.muted)
            if let port {
                Text("curl --proxy http://127.0.0.1:\(port) --cacert \"\(caPath)\" https://httpbin.org/json")
                    .font(Theme.mono)
                    .textSelection(.enabled)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            }
            Text("iOS Simulator: use Simulators → Trust Relay CA, then set this Mac's HTTP/HTTPS proxy to 127.0.0.1:\(port.map(String.init) ?? "8888").")
                .foregroundStyle(Theme.muted)
        }
        .padding(28)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

func formatMs(_ ms: UInt64) -> String {
    ms >= 1000 ? String(format: "%.2f s", Double(ms) / 1000) : "\(ms) ms"
}

func formatBytes(_ b: UInt64) -> String {
    if b >= 1_048_576 { return String(format: "%.1f MB", Double(b) / 1_048_576) }
    if b >= 1024 { return String(format: "%.1f KB", Double(b) / 1024) }
    return "\(b) B"
}
