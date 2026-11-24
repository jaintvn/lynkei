import AppKit
import LynkeiKit
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

    static let earWidth: CGFloat = 108
    private static let ear = earWidth
    private static let expandedWidth: CGFloat = 420

    /// Device chosen in the expanded island (nil: whichever is sending).
    var focusDevice: String?
    /// The device named in the compact island: the chosen one, else the one that sent the newest call.
    var focusedDevice: String {
        focusDevice ?? store.islandRows.last.map(Analytics.deviceLabel) ?? "Lynkei"
    }

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
        pinned = ProcessInfo.processInfo.environment["LYNKEI_ISLAND_OPEN"] != nil
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
        let tail = store.islandRows.suffix(60)
        for t in tail where t.state == .failed {
            guard let err = t.error, err.contains("may pin certificates"), !pinningNotified.contains(t.host) else { continue }
            pinningNotified.insert(t.host)
            post("lock.trianglebadge.exclamationmark", "Certificate pinning detected", "\(t.host) rejected Lynkei's certificate", Theme.failed, seconds: 8)
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
            post("iphone.slash", "Device disconnected", "\(deviceName(id, ds)) is no longer sending traffic through Lynkei", Theme.paused, seconds: 8)
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

    var recentFailures: Int { store.islandRows.filter(isFailure).count }
    var unseenFailures: Int { max(0, recentFailures - failuresSeen) }

    private func isFailure(_ t: FfiTransaction) -> Bool { t.state == .failed || (t.status ?? 0) >= 500 }

    /// Calls started in the last `seconds`, newest first.
    func calls(inLast seconds: Double) -> Int {
        let cutoff = UInt64(max(0, (Date().timeIntervalSince1970 - seconds) * 1000))
        var n = 0
        for t in store.islandRows.reversed() {
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
        return held
            ? CGSize(width: max(400, compactWidth), height: g.barHeight + 6 + 98)
            : CGSize(width: max(expandedWidth, compactWidth), height: g.barHeight + 6 + 286)
    }
}

/// Borderless panel that never takes focus from the app being debugged.
final class IslandPanel: NSPanel {
    /// The island is black glass whatever the system appearance, so its colours stay the dark ones.
    override var effectiveAppearance: NSAppearance { NSAppearance(named: .darkAqua)! }
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
                bottomLeadingRadius: island.expanded ? 26 : 14, bottomTrailingRadius: island.expanded ? 26 : 14
            )
            .fill(.black)
            .overlay {
                if island.expanded && !store.paused.isEmpty {
                    UnevenRoundedRectangle(bottomLeadingRadius: 26, bottomTrailingRadius: 26)
                        .stroke(Color(hex: 0xFF9F43).opacity(0.45), lineWidth: 1)
                }
            }
            if island.expanded {
                IslandExpanded(island: island, store: store)
                    .padding(.top, g.barHeight + 6).padding(.horizontal, 16).padding(.bottom, 16)
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

/// The part that shows beside the notch: the focused device on the left, the live rate and a failure badge on the right.
struct IslandCompact: View {
    let island: IslandController
    let store: TrafficStore

    var body: some View {
        let g = island.geometry
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            let rate = Double(island.calls(inLast: 5)) / 5
            let held = store.paused.count
            let device = island.focusedDevice
            HStack(spacing: 0) {
                HStack(spacing: 6) {
                    PlatformIcon(platform: Analytics.platform(ofDevice: device), size: 11, color: held > 0 ? Theme.paused : Theme.s2xx)
                    Text(device).font(.system(size: 12, weight: .semibold)).foregroundStyle(.white).lineLimit(1)
                }
                .frame(width: IslandController.earWidth - 14, alignment: .leading)
                .padding(.leading, 14)
                Spacer(minLength: g.notchWidth)
                HStack(spacing: 7) {
                    Circle().fill(held > 0 ? Theme.paused : (store.isRunning ? Theme.accent : Theme.s5xx))
                        .frame(width: 6, height: 6)
                        .overlay(Circle().stroke((held > 0 ? Theme.paused : Theme.accent).opacity(0.3), lineWidth: 3))
                    Text(held > 0 ? "\(held) held" : String(format: "%.0f/s", rate.rounded()))
                        .font(.system(size: 11.5, design: .monospaced)).foregroundStyle(.white)
                    if island.unseenFailures > 0 {
                        Text("\(island.unseenFailures)")
                            .font(.system(size: 10.5, weight: .bold))
                            .foregroundStyle(Color(hex: 0x0D0E11))
                            .padding(.horizontal, 4).frame(minWidth: 16, minHeight: 16)
                            .background(Capsule().fill(Color(hex: 0xFF6B70)))
                    }
                }
                .frame(width: IslandController.earWidth - 14, alignment: .trailing)
                .padding(.trailing, 14)
            }
            .frame(height: g.barHeight)
        }
    }
}

/// Hover or click: device tabs, the last five calls and one switch per tool. A held call replaces
/// them with the breakpoint card (Continue / Edit / Abort).
struct IslandExpanded: View {
    let island: IslandController
    let store: TrafficStore

    var body: some View {
        if let held = store.paused.first {
            breakpoint(held)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                deviceTabs
                calls
                toggles
            }
        }
    }

    private var deviceNames: [String] {
        let ds = store.deviceStore()
        var names = ds.devices.filter { ds.isConnected($0) }.map(\.name)
        for t in store.islandRows.suffix(200) { let n = Analytics.deviceLabel(t); if !names.contains(n) { names.append(n) } }
        return Array(names.prefix(3))
    }

    private var deviceTabs: some View {
        HStack(spacing: 6) {
            let names = deviceNames
            if names.isEmpty {
                Text(store.isRunning ? "Waiting for a device…" : "Lynkei is stopped")
                    .font(.system(size: 12)).foregroundStyle(.white.opacity(0.62)).frame(height: 34)
            }
            ForEach(names, id: \.self) { n in
                let on = island.focusedDevice == n
                let rate = store.islandRows.suffix(300).filter { Analytics.deviceLabel($0) == n && Double($0.startedAtMs) > (Date().timeIntervalSince1970 - 5) * 1000 }.count / 5
                Button { island.focusDevice = n } label: {
                    HStack(spacing: 7) {
                        PlatformIcon(platform: Analytics.platform(ofDevice: n), size: 11, color: .white)
                        Text(n).font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        Spacer(minLength: 0)
                        Text("\(rate)/s").font(.system(size: 11, design: .monospaced)).foregroundStyle(.white.opacity(0.62))
                    }
                    .padding(.horizontal, 10).frame(maxWidth: .infinity).frame(height: 34)
                    .background(RoundedRectangle(cornerRadius: 10).fill(on ? Color.white.opacity(0.10) : .clear))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.white.opacity(on ? 0.18 : 0.08)))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var calls: some View {
        let device = island.focusDevice
        let rows = store.islandRows.reversed().lazy.filter { device == nil || Analytics.deviceLabel($0) == device }.prefix(5)
        return VStack(spacing: 2) {
            if rows.isEmpty {
                Text("No calls yet").font(.system(size: 12)).foregroundStyle(.white.opacity(0.62)).frame(height: 26)
            }
            ForEach(Array(rows)) { tx in
                Button { island.open(tx) } label: {
                    HStack(spacing: 8) {
                        Text(statusText(tx)).font(.system(size: 11, weight: .bold, design: .monospaced))
                            .foregroundStyle(islandStatusColor(tx)).frame(width: 40, alignment: .leading)
                        Text(tx.method).font(.system(size: 11, design: .monospaced)).foregroundStyle(.white.opacity(0.62))
                            .frame(width: 42, alignment: .leading)
                        Text(tx.path).font(.system(size: 11.5, design: .monospaced)).lineLimit(1).truncationMode(.tail)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(verbatim: tx.state == .inProgress || tx.durationMs == 0 ? "—" : (tx.durationMs >= 1000 ? String(format: "%.2f s", Double(tx.durationMs) / 1000) : "\(tx.durationMs) ms"))
                            .font(.system(size: 11, design: .monospaced)).foregroundStyle(.white.opacity(0.7))
                            .frame(width: 50, alignment: .trailing)
                    }
                    .padding(.horizontal, 6).frame(height: 26)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func statusText(_ tx: FfiTransaction) -> String {
        switch tx.state {
        case .failed: return "ERR"
        case .tunneled: return "TUN"
        default: return tx.status.map(String.init) ?? "…"
        }
    }

    private func islandStatusColor(_ tx: FfiTransaction) -> Color {
        if tx.state == .failed { return Color(hex: 0xFF6B70) }
        switch (tx.status ?? 0) / 100 {
        case 2: return Color(hex: 0x3DD68C)
        case 3: return Color(hex: 0x9AACC6)
        case 4: return Color(hex: 0xF5B544)
        case 5: return Color(hex: 0xFF6B70)
        default: return .white.opacity(0.62)
        }
    }

    /// Proxy, SSL, Breakpoints, Rewrite, Map Local, Throttle.
    private var toggles: some View {
        func rulesOn(_ kind: String) -> Bool { store.rules.contains { $0.kind == kind && $0.enabled } }
        func flip(_ kind: String) {
            let rules = store.rules.filter { $0.kind == kind }
            let on = rules.contains(where: \.enabled)
            rules.forEach { store.setRule($0.id, enabled: !on) }
        }
        let items: [(String, Bool, Color, () -> Void)] = [
            ("Proxy", store.isRunning, Color(hex: 0x7C8CFF), { store.isRunning ? store.stop() : store.start() }),
            ("SSL", store.decrypt, Color(hex: 0x3DD68C), { store.setDecrypt(!store.decrypt) }),
            ("Pause", rulesOn("breakpoint"), Color(hex: 0xFF9F43), { flip("breakpoint") }),
            ("Modify", rulesOn("rewrite"), Color(hex: 0x3DD68C), { flip("rewrite") }),
            ("Mock", rulesOn("map local"), Color(hex: 0x3DD68C), { flip("map local") }),
            ("Slow network", store.globalThrottle != nil, Color(hex: 0x3DD68C), { store.setGlobalThrottle(store.globalThrottle == nil ? "3g" : nil) }),
        ]
        return LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 3), spacing: 6) {
            ForEach(items, id: \.0) { label, on, dot, action in
                Button(action: action) {
                    HStack(spacing: 7) {
                        Circle().fill(on ? dot : Color.white.opacity(0.25)).frame(width: 7, height: 7)
                        Text(label).font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .foregroundStyle(on ? Color(hex: 0xF2F3F6) : .white.opacity(0.62))
                    .padding(.horizontal, 10).frame(height: 34)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Color.white.opacity(on ? 0.12 : 0.04)))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func breakpoint(_ p: FfiPaused) -> some View {
        let path = URL(string: p.url).map { $0.path.isEmpty ? "/" : $0.path } ?? p.url
        return VStack(spacing: 12) {
            HStack(spacing: 10) {
                Image(systemName: "pause.fill").font(.system(size: 13)).foregroundStyle(Color(hex: 0xFF9F43))
                    .frame(width: 34, height: 34)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Color(hex: 0xFF9F43).opacity(0.18)))
                VStack(alignment: .leading, spacing: 2) {
                    Text("Paused · \(island.focusedDevice) · \(p.response ? "response" : "request")")
                        .font(.system(size: 11.5, weight: .semibold)).foregroundStyle(Color(hex: 0xFF9F43)).lineLimit(1)
                    Text("\(p.method) \(path)").font(.system(size: 13, weight: .semibold, design: .monospaced))
                        .lineLimit(1).truncationMode(.middle)
                }
                Spacer(minLength: 0)
                if store.paused.count > 1 {
                    Text("+\(store.paused.count - 1)").font(.system(size: 11.5, design: .monospaced)).foregroundStyle(.white.opacity(0.62))
                }
            }
            HStack(spacing: 6) {
                bpButton("Continue", fill: Color(hex: 0x7C8CFF), fg: Color(hex: 0x0D0E11)) { store.resumePaused(p, edit: nil) }
                bpButton("Edit", fill: Color.white.opacity(0.10), fg: Color(hex: 0xF2F3F6)) { island.showMainWindow() }
                bpButton("Abort", fill: Color(hex: 0xFF6B70).opacity(0.16), fg: Color(hex: 0xFF8A8E)) { store.abortPaused(p) }
            }
        }
    }

    private func bpButton(_ title: String, fill: Color, fg: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.system(size: 13, weight: .semibold)).foregroundStyle(fg)
                .frame(maxWidth: .infinity).frame(height: 36)
                .background(RoundedRectangle(cornerRadius: 10).fill(fill))
        }
        .buttonStyle(.plain)
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

struct LynkeiMenuBar: View {
    @Bindable var island: IslandController
    let store: TrafficStore

    var body: some View {
        Picker("Island", selection: $island.mode) {
            ForEach(IslandController.Mode.allCases) { Text($0.title).tag($0) }
        }
        .pickerStyle(.inline)
        Divider()
        Toggle("Decrypt HTTPS", isOn: Binding(get: { store.decrypt }, set: { store.setDecrypt($0) }))
        Picker("Slow the whole network", selection: Binding(get: { store.globalThrottle ?? "" }, set: { store.setGlobalThrottle($0.isEmpty ? nil : $0) })) {
            Text("Off").tag("")
            ForEach(store.throttlePresets, id: \.self) { Text($0 == "offline" ? "Offline" : $0.uppercased()).tag($0) }
        }
        Divider()
        Button(store.isRunning ? "Stop recording" : "Start recording") { store.isRunning ? store.stop() : store.start() }
        Button("Show Lynkei") { island.showMainWindow() }
        Divider()
        Button("Quit Lynkei") { NSApp.terminate(nil) }
    }
}
