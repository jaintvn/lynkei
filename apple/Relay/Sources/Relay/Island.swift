import AppKit
import RelayKit
import SwiftUI

/// The notch "island": a small black pill that hugs the notch (or floats at top-centre on Macs
/// without one), shows live traffic at a glance, expands on hover with the latest calls and rule
/// toggles, and takes over when a breakpoint is holding a request.
@MainActor
@Observable
final class IslandController {
    enum Mode: String, CaseIterable, Identifiable {
        case always, activity, off
        var id: String { rawValue }
        var title: String {
            switch self {
            case .always: return "Always"
            case .activity: return "Only during activity"
            case .off: return "Off"
            }
        }
    }

    var mode: Mode {
        didSet {
            UserDefaults.standard.set(mode.rawValue, forKey: "islandMode")
            refresh()
        }
    }
    /// Current size of the pill; the panel follows it.
    private(set) var size = CGSize(width: 220, height: 32)
    private(set) var expanded = false
    private(set) var geometry = Geometry(hasNotch: false, notchWidth: 170, barHeight: 30)
    /// How many failures the user has already seen (expanding the island marks them seen).
    private(set) var failuresSeen = 0

    /// A short notice that pulses under the notch for a few seconds.
    struct Alert: Equatable, Identifiable {
        let id = UUID()
        var symbol: String
        var title: String
        var detail: String
        var color: Color
        var until: Date
    }

    private(set) var alert: Alert?

    struct Geometry: Equatable {
        var hasNotch: Bool
        var notchWidth: CGFloat
        var barHeight: CGFloat
    }

    @ObservationIgnored private let store: TrafficStore
    @ObservationIgnored private var panel: IslandPanel?
    @ObservationIgnored private var hovering = false
    @ObservationIgnored private var pinned = false
    @ObservationIgnored private var collapseTask: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private var deviceWatcher: Task<Void, Never>?
    @ObservationIgnored private var pinningNotified: Set<String> = []
    @ObservationIgnored private var lastSpikeAlert = Date.distantPast
    @ObservationIgnored private var knownConnected: Set<String>?

    private static let ear: CGFloat = 58
    private static let expandedWidth: CGFloat = 460

    init(store: TrafficStore) {
        self.store = store
        mode = Mode(rawValue: UserDefaults.standard.string(forKey: "islandMode") ?? "") ?? .always
    }

    /// Creates the panel and starts following traffic. Safe to call more than once.
    func start() {
        guard panel == nil else { return }
        let p = IslandPanel(
            contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = false
        p.level = .statusBar
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        p.isMovable = false
        p.hidesOnDeactivate = false
        let host = NSHostingView(rootView: IslandView(island: self, store: store))
        host.sizingOptions = []
        p.contentView = host
        panel = p
        // For demos and screenshots: start with the island open.
        pinned = ProcessInfo.processInfo.environment["RELAY_ISLAND_OPEN"] != nil
        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in self?.refresh() } }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                self?.refresh()
                try? await Task.sleep(nanoseconds: 300_000_000)
            }
        }
        deviceWatcher = Task { [weak self] in
            while !Task.isCancelled {
                await self?.checkDevices()
                try? await Task.sleep(for: .seconds(6))
            }
        }
        refresh()
    }

    // MARK: Alerts

    private func post(_ symbol: String, _ title: String, _ detail: String, _ color: Color, seconds: Double = 6) {
        alert = Alert(symbol: symbol, title: title, detail: detail, color: color, until: Date().addingTimeInterval(seconds))
        refresh()
    }

    /// Looks at the newest calls for things worth a pulse: pinning, and a burst of failures.
    private func scanForAlerts() {
        if let a = alert, a.until < Date() { alert = nil }
        let tail = store.transactions.suffix(60)
        for t in tail where t.state == .failed {
            guard let err = t.error, err.contains("may pin certificates"), !pinningNotified.contains(t.host) else { continue }
            pinningNotified.insert(t.host)
            post("lock.trianglebadge.exclamationmark", "Certificate pinning detected", "\(t.host) rejected Relay's certificate", Theme.failed, seconds: 8)
        }
        let cutoff = UInt64(max(0, (Date().timeIntervalSince1970 - 10) * 1000))
        let recent = tail.filter { $0.startedAtMs >= cutoff && isFailure($0) && !($0.error ?? "").contains("may pin certificates") }
        if recent.count >= 3, Date().timeIntervalSince(lastSpikeAlert) > 60 {
            lastSpikeAlert = Date()
            let hosts = Set(recent.map(\.host))
            post("exclamationmark.triangle.fill", "Failures spiking", "\(recent.count) failed in 10 s" + (hosts.count == 1 ? " on \(hosts.first!)" : " across \(hosts.count) hosts"), Theme.s5xx)
        }
    }

    /// Polls devices (a few seconds apart) and warns when a connected one drops away.
    private func checkDevices() async {
        guard store.isRunning else { return }
        let ds = store.deviceStore()
        await ds.refresh()
        let connected = Set(ds.devices.filter { ds.isConnected($0) }.map(\.id))
        defer { knownConnected = connected }
        guard let before = knownConnected else { return }
        for id in before.subtracting(connected) where !ds.userDisconnected.contains(id) {
            post("iphone.slash", "Device disconnected", "\(deviceName(id, ds)) is no longer sending traffic through Relay", Theme.paused, seconds: 8)
        }
    }

    private func deviceName(_ id: String, _ ds: DeviceStore) -> String {
        ds.devices.first { $0.id == id }?.name ?? "A device"
    }

    // MARK: Behaviour

    func hover(_ inside: Bool) {
        hovering = inside
        collapseTask?.cancel()
        if inside {
            refresh()
        } else {
            collapseTask = Task { [weak self] in
                try? await Task.sleep(nanoseconds: 350_000_000)
                guard !Task.isCancelled else { return }
                self?.pinned = false
                self?.refresh()
            }
        }
    }

    func togglePinned() {
        pinned.toggle()
        refresh()
    }

    /// Shows a call in the main window.
    func open(_ tx: FfiTransaction) {
        store.selection = tx.id
        showMainWindow()
    }

    func showMainWindow() {
        NSApp.activate(ignoringOtherApps: true)
        for w in NSApp.windows where w !== panel && w.canBecomeMain {
            w.makeKeyAndOrderFront(nil)
            break
        }
    }

    var recentFailures: Int { store.transactions.filter(isFailure).count }
    var unseenFailures: Int { max(0, recentFailures - failuresSeen) }

    private func isFailure(_ t: FfiTransaction) -> Bool { t.state == .failed || (t.status ?? 0) >= 500 }

    /// Calls started in the last `seconds`, newest first.
    func calls(inLast seconds: Double) -> Int {
        let cutoff = UInt64(max(0, (Date().timeIntervalSince1970 - seconds) * 1000))
        var n = 0
        for t in store.transactions.reversed() {
            if t.startedAtMs < cutoff { break }
            n += 1
        }
        return n
    }

    // MARK: Layout

    private func refresh() {
        guard let panel else { return }
        scanForAlerts()
        let held = !store.paused.isEmpty
        let busy = calls(inLast: 6) > 0
        let visible: Bool
        switch mode {
        case .always: visible = true
        case .activity: visible = busy || held || hovering || pinned || expanded || alert != nil
        case .off: visible = false
        }
        let wantExpanded = visible && (hovering || pinned || held)
        guard visible, let screen = Self.targetScreen() else {
            if panel.isVisible { panel.orderOut(nil) }
            expanded = false
            return
        }
        let g = Self.geometry(of: screen)
        if wantExpanded && !expanded { failuresSeen = recentFailures }
        if wantExpanded { failuresSeen = recentFailures }
        let deviceRows = min(3, store.deviceStore().devices.count)
        let target = Self.targetSize(g, expanded: wantExpanded, held: held, deviceRows: deviceRows, alert: alert != nil)
        let changed = target != size || g != geometry || wantExpanded != expanded || !panel.isVisible
        guard changed else { return }
        let animate = panel.isVisible
        withAnimation(.spring(duration: 0.32, bounce: 0.18)) {
            geometry = g
            expanded = wantExpanded
            size = target
        }
        let top: CGFloat = g.hasNotch ? 0 : 6
        let frame = NSRect(
            x: screen.frame.midX - target.width / 2, y: screen.frame.maxY - target.height - top,
            width: target.width, height: target.height)
        panel.setFrame(frame, display: true, animate: animate)
        panel.orderFrontRegardless()
    }

    private static func targetScreen() -> NSScreen? {
        NSScreen.screens.first { $0.safeAreaInsets.top > 0 } ?? NSScreen.main ?? NSScreen.screens.first
    }

    private static func geometry(of screen: NSScreen) -> Geometry {
        let inset = screen.safeAreaInsets.top
        if inset > 0, let left = screen.auxiliaryTopLeftArea, let right = screen.auxiliaryTopRightArea {
            return Geometry(hasNotch: true, notchWidth: screen.frame.width - left.width - right.width, barHeight: inset)
        }
        return Geometry(hasNotch: false, notchWidth: 150, barHeight: 30)
    }

    private static func targetSize(_ g: Geometry, expanded: Bool, held: Bool, deviceRows: Int, alert: Bool) -> CGSize {
        let compactWidth = g.notchWidth + 2 * ear
        guard expanded else {
            // An alert drops a notice line under the notch.
            return alert
                ? CGSize(width: max(380, compactWidth), height: g.barHeight + 46)
                : CGSize(width: compactWidth, height: g.barHeight)
        }
        let devices: CGFloat = deviceRows > 0 ? 30 + CGFloat(deviceRows) * 28 : 0
        return CGSize(width: max(expandedWidth, compactWidth), height: g.barHeight + (held ? 372 : 292) + devices)
    }
}

/// Borderless panel that never takes focus from the app being debugged.
final class IslandPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

// MARK: - View

struct IslandView: View {
    let island: IslandController
    let store: TrafficStore

    var body: some View {
        let g = island.geometry
        ZStack(alignment: .top) {
            UnevenRoundedRectangle(
                bottomLeadingRadius: island.expanded ? 24 : 14, bottomTrailingRadius: island.expanded ? 24 : 14
            )
            .fill(.black)
            if island.expanded {
                IslandExpanded(island: island, store: store).padding(.top, g.barHeight + 6)
                    .transition(.opacity)
            } else {
                VStack(spacing: 0) {
                    IslandCompact(island: island, store: store)
                    if let a = island.alert { IslandAlertBanner(alert: a).transition(.opacity) }
                }
            }
        }
        .frame(width: island.size.width, height: island.size.height, alignment: .top)
        .contentShape(Rectangle())
        .onHover { island.hover($0) }
        .onTapGesture { island.togglePinned() }
        .preferredColorScheme(.dark)
    }
}

/// The part that shows beside the notch: a pulse on the left, a count or alert on the right.
struct IslandCompact: View {
    let island: IslandController
    let store: TrafficStore

    var body: some View {
        let g = island.geometry
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            let rate = island.calls(inLast: 5)
            let held = store.paused.count
            HStack(spacing: 0) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(held > 0 ? Theme.paused : (store.isRunning ? (rate > 0 ? Theme.s2xx : Theme.faint) : Theme.s5xx))
                        .frame(width: 7, height: 7)
                        .shadow(color: held > 0 ? Theme.paused : Theme.s2xx.opacity(rate > 0 ? 0.9 : 0), radius: 4)
                    if held > 0 {
                        Text("\(held) held").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.paused)
                    } else if rate > 0 {
                        Text("\(rate)").font(.system(size: 11, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.muted)
                    }
                }
                .frame(width: 58, alignment: .center)
                Spacer(minLength: g.notchWidth)
                Group {
                    if island.unseenFailures > 0 {
                        Text("\(island.unseenFailures)")
                            .font(.system(size: 10.5, weight: .bold, design: .monospaced))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 6).padding(.vertical, 1.5)
                            .background(Capsule().fill(Theme.s5xx))
                    } else {
                        Image(systemName: "arrow.triangle.swap").font(.system(size: 10.5, weight: .semibold)).foregroundStyle(Theme.faint)
                    }
                }
                .frame(width: 58, alignment: .center)
            }
            .frame(height: g.barHeight)
        }
    }
}

/// Hover or click: the latest calls, rule toggles, and the breakpoint card when something is held.
struct IslandExpanded: View {
    let island: IslandController
    let store: TrafficStore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let held = store.paused.first { breakpoint(held) }
            VStack(alignment: .leading, spacing: 2) {
                Text("LATEST CALLS").font(.system(size: 10, weight: .semibold)).foregroundStyle(Theme.faint)
                let latest = Array(store.transactions.suffix(5).reversed())
                if latest.isEmpty {
                    Text(store.isRunning ? "Waiting for traffic…" : "Relay is stopped")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.vertical, 8)
                }
                ForEach(latest) { tx in
                    Button { island.open(tx) } label: { callRow(tx) }.buttonStyle(.plain)
                }
            }
            devicesSection
            Spacer(minLength: 0)
            toggles
        }
        .padding(.horizontal, 18)
        .padding(.bottom, 16)
    }

    /// Simulators and phones Relay can see, with a one-tap connect or disconnect.
    @ViewBuilder private var devicesSection: some View {
        let ds = store.deviceStore()
        let shown = Array(ds.devices.prefix(3))
        if !shown.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("DEVICES").font(.system(size: 10, weight: .semibold)).foregroundStyle(Theme.faint)
                ForEach(shown, id: \.id) { d in
                    let on = ds.isConnected(d)
                    HStack(spacing: 8) {
                        Image(systemName: d.kind.hasPrefix("ios") ? "iphone" : "smartphone")
                            .foregroundStyle(on ? Theme.s2xx : Theme.faint).frame(width: 18)
                        Text(d.name).font(.system(size: 12, weight: .medium)).lineLimit(1)
                        Text(d.os).font(.system(size: 11)).foregroundStyle(Theme.faint).lineLimit(1)
                        Spacer()
                        if ds.busy.contains(d.id) {
                            ProgressView().controlSize(.mini)
                        } else {
                            Button(on ? "Disconnect" : "Connect") {
                                Task { on ? await ds.disconnect(d) : await ds.connect(d) }
                            }
                            .buttonStyle(IslandChip(on: !on))
                        }
                    }
                }
                if ds.devices.count > shown.count {
                    Text("+\(ds.devices.count - shown.count) more in Connect device").font(.system(size: 11)).foregroundStyle(Theme.faint)
                }
            }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.triangle.swap").foregroundStyle(Theme.accent)
            Text("Relay").font(.system(size: 13, weight: .semibold))
            Text(store.isRunning ? "Recording" : "Stopped").font(.system(size: 12)).foregroundStyle(Theme.muted)
            Spacer()
            Button(store.isRunning ? "Stop" : "Start") { store.isRunning ? store.stop() : store.start() }
                .buttonStyle(IslandChip(on: store.isRunning))
            Button("Open") { island.showMainWindow() }.buttonStyle(IslandChip(on: false))
        }
    }

    private func breakpoint(_ p: FfiPaused) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "pause.circle.fill").foregroundStyle(Theme.paused)
                Text(p.response ? "Response held" : "Request held").font(.system(size: 12, weight: .semibold))
                Text("rule \(p.rule)").font(.system(size: 11)).foregroundStyle(Theme.muted)
                if store.paused.count > 1 { Text("+\(store.paused.count - 1) more").font(.system(size: 11)).foregroundStyle(Theme.muted) }
            }
            Text("\(p.method) \(p.url)").font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.fg)
                .lineLimit(2).truncationMode(.middle)
            HStack(spacing: 8) {
                Button("Continue") { store.resumePaused(p, edit: nil) }.buttonStyle(IslandChip(on: true))
                Button("Edit…") { island.showMainWindow() }.buttonStyle(IslandChip(on: false))
                Button("Abort") { store.abortPaused(p) }.buttonStyle(IslandChip(on: false, danger: true))
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12).fill(Theme.paused.opacity(0.13)))
    }

    private func callRow(_ tx: FfiTransaction) -> some View {
        HStack(spacing: 8) {
            StatusPill(tx: tx).frame(width: 50, alignment: .leading)
            Text(tx.method).font(Theme.monoSmall).foregroundStyle(Theme.muted).frame(width: 46, alignment: .leading)
            Text("\(tx.host)\(tx.path)").font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.fg)
                .lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 4)
            Text(tx.state == .inProgress ? "…" : "\(tx.durationMs) ms")
                .font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.faint)
        }
        .padding(.vertical, 3)
        .contentShape(Rectangle())
    }

    /// One switch per kind of rule that exists (all rules of that kind on or off together).
    @ViewBuilder private var toggles: some View {
        let kinds = Dictionary(grouping: store.rules, by: \.kind)
        if kinds.isEmpty {
            Text("No rules yet. Right-click a request in the main window to add one.")
                .font(.system(size: 11)).foregroundStyle(Theme.faint)
        } else {
            HStack(spacing: 6) {
                ForEach(kinds.keys.sorted(), id: \.self) { kind in
                    let rules = kinds[kind] ?? []
                    let on = rules.allSatisfy(\.enabled)
                    Button(kind.capitalized) { rules.forEach { store.setRule($0.id, enabled: !on) } }
                        .buttonStyle(IslandChip(on: on))
                        .help(on ? "Turn off \(rules.count) \(kind) rule(s)" : "Turn on \(rules.count) \(kind) rule(s)")
                }
                Spacer()
            }
        }
    }
}

/// The notice line that drops under the notch for a few seconds.
struct IslandAlertBanner: View {
    let alert: IslandController.Alert
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: alert.symbol).foregroundStyle(alert.color)
            VStack(alignment: .leading, spacing: 1) {
                Text(alert.title).font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.fg)
                Text(alert.detail).font(.system(size: 11)).foregroundStyle(Theme.muted).lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .padding(.top, 4)
    }
}

struct IslandChip: ButtonStyle {
    let on: Bool
    var danger = false
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11.5, weight: .semibold))
            .foregroundStyle(danger ? Theme.s5xx : (on ? Color.black : Theme.fg))
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(Capsule().fill(danger ? Theme.s5xx.opacity(0.16) : (on ? Theme.accent : Color.white.opacity(0.1))))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

// MARK: - Menu bar

struct RelayMenuBar: View {
    @Bindable var island: IslandController
    let store: TrafficStore

    var body: some View {
        Picker("Island", selection: $island.mode) {
            ForEach(IslandController.Mode.allCases) { Text($0.title).tag($0) }
        }
        .pickerStyle(.inline)
        Divider()
        Button(store.isRunning ? "Stop recording" : "Start recording") { store.isRunning ? store.stop() : store.start() }
        Button("Show Relay") { island.showMainWindow() }
        Divider()
        Button("Quit Relay") { NSApp.terminate(nil) }
    }
}
