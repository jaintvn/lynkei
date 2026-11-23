import RelayKit
import SwiftUI

/// Dashboard (canvas board 2): KPIs, failing and slow endpoints, active rules, top hosts, live calls.
struct DashboardView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(Sheets.self) private var sheets
    @Binding var screen: Screen

    var body: some View {
        VStack(spacing: 0) {
            AppHeader(screen: $screen) {
                Spacer(minLength: 0)
                FocusToggle()
                deviceChips
            }
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                let m = Metrics(store.scope(store.transactions))
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        title(m)
                        kpis(m)
                        EqualColumns(spans: [1, 1, 1]) {
                            failing(m)
                            slowest(m)
                            activeRules
                        }
                        EqualColumns(spans: [1, 2]) {
                            topHosts(m)
                            live(m)
                        }
                    }
                    .padding(.horizontal, 24).padding(.top, 22).padding(.bottom, 28)
                    .frame(maxWidth: 1440)
                    .frame(maxWidth: .infinity)
                }
            }
        }
    }

    // MARK: Pieces

    /// Devices that are connected or have sent traffic; the one with the newest call is the active one.
    private var deviceNames: [String] {
        let ds = store.deviceStore()
        var names = ds.devices.filter { ds.isConnected($0) }.map(\.name)
        for t in store.transactions.suffix(300) { let n = Analytics.deviceLabel(t); if !names.contains(n) { names.append(n) } }
        return names
    }

    private var activeDevice: String? {
        store.transactions.last.map(Analytics.deviceLabel) ?? deviceNames.first
    }

    private var deviceChips: some View {
        let ds = store.deviceStore()
        let names = deviceNames
        return HStack(spacing: 6) {
            chip("All devices", on: store.deviceFilter == nil) { store.deviceFilter = nil }
            ForEach(names, id: \.self) { n in
                chip(n, on: store.deviceFilter == n, live: n == activeDevice) { store.deviceFilter = store.deviceFilter == n ? nil : n }
            }
            if names.isEmpty { chip("Connect device…", on: false) { sheets.showConnect = true } }
            Menu {
                Button("All apps") { store.appFilter = nil }
                if !store.processesSeen.isEmpty { Divider() }
                ForEach(store.processesSeen, id: \.name) { p in Button("\(p.name)  (\(p.count))") { store.appFilter = p.name } }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "app.dashed").font(.system(size: 11))
                    Text(store.appFilter ?? "All apps").font(.system(size: 12.5, weight: store.appFilter == nil ? .regular : .medium))
                    Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold)).foregroundStyle(Theme.muted)
                }
                .padding(.horizontal, 11).frame(height: 30)
                .background(RoundedRectangle(cornerRadius: 8).fill(store.appFilter == nil ? .clear : Theme.selection))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(store.appFilter == nil ? Theme.line2 : Theme.accent))
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        }
        .onAppear { ds.startWatching() }
    }

    private func chip(_ label: String, on: Bool, live: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 7) {
                if label == "All devices" || label == "Connect device…" {
                    Circle().fill(Theme.s2xx).frame(width: 7, height: 7)
                } else {
                    PlatformIcon(platform: Analytics.platform(ofDevice: label), size: 12, color: Theme.fg)
                    Circle().fill(Theme.s2xx).frame(width: 6, height: 6)
                        .shadow(color: live ? Theme.s2xx.opacity(0.8) : .clear, radius: 4)
                }
                Text(label).font(.system(size: 12.5, weight: on ? .medium : .regular))
            }
            .padding(.horizontal, 11).frame(height: 30)
            .background(RoundedRectangle(cornerRadius: 8).fill(on ? Theme.selection : .clear))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(on ? Theme.accent : Theme.line2))
        }
        .buttonStyle(.plain)
    }

    private func title(_ m: Metrics) -> some View {
        let app = store.appFilter ?? m.mainApp
        let device = store.deviceFilter ?? activeDevice
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(app.map { "\($0) · live" } ?? "Live traffic").font(.system(size: 22, weight: .semibold)).tracking(-0.4)
                Text("Last 15 minutes · \(m.window.count.formatted()) requests")
                    .foregroundStyle(Theme.muted)
            }
            HStack(spacing: 14) {
                if let device {
                    Label { Text("Active device ").foregroundStyle(Theme.muted) + Text(device).fontWeight(.medium) } icon: {
                        Circle().fill(Theme.s2xx).frame(width: 7, height: 7)
                    }
                }
                if let app {
                    Label { Text("App ").foregroundStyle(Theme.muted) + Text(app).fontWeight(.medium) } icon: {
                        Image(systemName: "app.dashed").font(.system(size: 11)).foregroundStyle(Theme.muted)
                    }
                }
            }
            .font(.system(size: 12.5))
        }
    }

    private func kpis(_ m: Metrics) -> some View {
        EqualColumns(spans: [1, 1, 1, 1]) {
            kpi("Requests / sec", String(format: "%.1f", m.rps), m.rpsDelta, Theme.muted, Theme.accent, m.rpsSpark)
            kpi("Error rate", String(format: "%.1f%%", m.errorRate * 100), m.errorDelta, m.errorRate > 0.01 ? Theme.s5xx : Theme.muted,
                m.errorRate > 0.01 ? Theme.s5xx : Theme.s2xx, m.errSpark)
            kpi("p95 latency", formatMs(m.p95), m.slowestNote, m.p95 > 800 ? Theme.s4xx : Theme.muted, m.p95 > 800 ? Theme.s4xx : Theme.accent, m.latSpark)
            kpi("Data transferred", formatBytes(m.bytes), m.imageShare, Theme.muted, Theme.s2xx, m.bytesSpark)
        }
    }

    private func kpi(_ label: String, _ value: String, _ delta: String, _ deltaColor: Color, _ color: Color, _ spark: [Double]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(label).font(.system(size: 12)).foregroundStyle(Theme.muted)
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(value).font(.system(size: 28, weight: .semibold)).tracking(-0.5).foregroundStyle(color == Theme.accent ? Theme.fg : color)
                Text(delta).font(.system(size: 12)).foregroundStyle(deltaColor).lineLimit(1)
            }
            Sparkline(values: spark, color: color, lineWidth: 2).frame(height: 40)
        }
        .card()
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func failing(_ m: Metrics) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Failing endpoints").font(.system(size: 13, weight: .semibold))
                Spacer()
                Button("All endpoints") { screen = .endpoints }.buttonStyle(.plain).font(.system(size: 12)).foregroundStyle(Theme.accent)
            }
            if m.failing.isEmpty { Text("Nothing is failing right now.").font(.system(size: 12)).foregroundStyle(Theme.muted) }
            ForEach(m.failing.prefix(4)) { e in
                let c = e.errorRate >= 0.5 || e.tunnel ? Theme.s5xx : Theme.s4xx
                HStack(spacing: 10) {
                    Text(e.tunnel ? "ERR" : (e.lastStatus.map(String.init) ?? "ERR"))
                        .font(.system(size: 11, weight: .semibold, design: .monospaced)).foregroundStyle(c)
                        .padding(.horizontal, 7).padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 5).fill(c.opacity(0.14)))
                        .frame(width: 52, alignment: .leading)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(e.method) \(e.path)").font(.system(size: 12, design: .monospaced)).lineLimit(1)
                        Text(e.host).font(.system(size: 11.5)).foregroundStyle(Theme.muted).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Text("\(Int((e.errorRate * 100).rounded()))%").font(.system(size: 12, design: .monospaced)).foregroundStyle(c)
                }
                .padding(.vertical, 8)
                .overlay(alignment: .top) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .card().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func slowest(_ m: Metrics) -> some View {
        let top = m.slowest.prefix(5)
        let mx = Double(top.first?.p95 ?? 1)
        return VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 4) {
                Text("Slowest endpoints").font(.system(size: 13, weight: .semibold))
                Text("p95").font(.system(size: 13)).foregroundStyle(Theme.muted)
            }
            if top.isEmpty { Text("No calls yet.").font(.system(size: 12)).foregroundStyle(Theme.muted) }
            ForEach(Array(top)) { e in
                let c = e.p95 > 800 ? Theme.s4xx : Theme.accent
                HStack(spacing: 10) {
                    Text("\(e.method) \(e.path)").font(.system(size: 12, design: .monospaced)).lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    MeterBar(fraction: Double(e.p95) / max(mx, 1), color: c).frame(width: 120)
                    Text(formatMs(e.p95)).font(.system(size: 12, design: .monospaced)).foregroundStyle(c).frame(width: 62, alignment: .trailing)
                }
                .padding(.vertical, 6)
            }
        }
        .card().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var activeRules: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Active rules").font(.system(size: 13, weight: .semibold))
            let active = store.rules.filter(\.enabled)
            if active.isEmpty { Text("No rules switched on.").font(.system(size: 12)).foregroundStyle(Theme.muted) }
            ForEach(active.prefix(5), id: \.id) { r in
                HStack(spacing: 10) {
                    RuleLetter(kind: r.kind, size: 22)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(RuleStyle.group(r.kind)).font(.system(size: 13, weight: .medium))
                        Text(r.detail.isEmpty ? r.summary : r.detail).font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.muted).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    let hits = store.transactions.filter { $0.appliedRules.contains(r.id) }.count
                    Text(hits == 0 ? "on" : "\(hits) hit\(hits == 1 ? "" : "s")").font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.muted)
                    RelaySwitch(isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                }
                .padding(.vertical, 8)
                .overlay(alignment: .top) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .card().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func topHosts(_ m: Metrics) -> some View {
        let top = m.hosts.prefix(5)
        let mx = Double(top.first?.count ?? 1)
        return VStack(alignment: .leading, spacing: 10) {
            Text("Top hosts").font(.system(size: 13, weight: .semibold))
            if top.isEmpty { Text("No calls yet.").font(.system(size: 12)).foregroundStyle(Theme.muted) }
            ForEach(Array(top), id: \.host) { h in
                HStack(spacing: 10) {
                    Text(h.host).font(.system(size: 12.5)).lineLimit(1).frame(width: 200, alignment: .leading)
                    MeterBar(fraction: Double(h.count) / mx, color: Theme.accent, height: 8)
                    Text("\(h.count)").font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.muted).frame(width: 48, alignment: .trailing)
                }
            }
        }
        .card().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func live(_ m: Metrics) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Circle().fill(Theme.s5xx).frame(width: 8, height: 8).shadow(color: Theme.s5xx.opacity(0.4), radius: 4)
                Text("Live").font(.system(size: 13, weight: .semibold))
                Spacer()
                Button("Open traffic") { screen = .traffic }.buttonStyle(.plain).font(.system(size: 12)).foregroundStyle(Theme.accent)
            }
            if m.latest.isEmpty { Text("Waiting for traffic…").font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.vertical, 6) }
            ForEach(m.latest) { t in
                HStack(spacing: 10) {
                    Text(Self.clock(t.startedAtMs)).font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.faint).lineLimit(1).fixedSize().frame(width: 96, alignment: .leading)
                    StatusPill(tx: t).frame(width: 52, alignment: .leading)
                    Text(t.method).font(.system(size: 11.5, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.muted).frame(width: 60, alignment: .leading)
                    Text(t.state == .tunneled ? "\(t.host):\(t.port)" : t.path).font(.system(size: 12, design: .monospaced)).lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Text(Analytics.deviceLabel(t)).font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1).frame(width: 110, alignment: .leading)
                    Text(t.durationMs == 0 ? "—" : formatMs(t.durationMs)).font(.system(size: 12, design: .monospaced)).frame(width: 64, alignment: .trailing)
                }
                .frame(height: 30)
                .overlay(alignment: .top) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .card().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private static func clock(_ ms: UInt64) -> String {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }
}

/// Everything the Dashboard shows, worked out once per refresh from the last 15 minutes of calls.
private struct Metrics {
    let window: [FfiTransaction]
    let rps: Double
    let rpsDelta: String
    let rpsSpark: [Double]
    let errorRate: Double
    let errorDelta: String
    let errSpark: [Double]
    let p95: UInt64
    let slowestNote: String
    let latSpark: [Double]
    let bytes: UInt64
    let imageShare: String
    let bytesSpark: [Double]
    let failing: [DashEndpoint]
    let slowest: [DashEndpoint]
    let hosts: [(host: String, count: Int)]
    let latest: [FfiTransaction]
    let mainApp: String?

    init(_ all: [FfiTransaction]) {
        let now = Analytics.nowMs()
        let span: UInt64 = 15 * 60 * 1000
        let win = all.filter { now >= $0.startedAtMs && now - $0.startedAtMs <= span }
        // No traffic in the last 15 minutes: show what there is rather than zeros.
        let w = win.isEmpty ? Array(all.suffix(500)) : win
        window = w
        let buckets = 14
        func bucketed(_ f: ([FfiTransaction]) -> Double) -> [Double] {
            let size = Double(span) / Double(buckets)
            var groups = [[FfiTransaction]](repeating: [], count: buckets)
            for t in w {
                let age = now > t.startedAtMs ? Double(now - t.startedAtMs) : 0
                groups[max(0, min(buckets - 1, buckets - 1 - Int(age / size)))].append(t)
            }
            return groups.map { $0.isEmpty ? 0 : f($0) }
        }
        let lastMinute = all.filter { now >= $0.startedAtMs && now - $0.startedAtMs <= 60_000 }.count
        rps = Double(lastMinute) / 60
        let recent5 = all.filter { now >= $0.startedAtMs && now - $0.startedAtMs <= 300_000 }.count
        let prev5 = all.filter { now >= $0.startedAtMs && now - $0.startedAtMs > 300_000 && now - $0.startedAtMs <= 600_000 }.count
        rpsDelta = prev5 == 0 ? (recent5 == 0 ? "idle" : "\(recent5) in the last 5 min")
            : String(format: "%+.0f%% vs 5 min ago", (Double(recent5) / Double(prev5) - 1) * 100)
        rpsSpark = bucketed { Double($0.count) }

        let errs = window.filter(Analytics.isError).count
        errorRate = window.isEmpty ? 0 : Double(errs) / Double(window.count)
        errorDelta = "\(errs) of \(window.count) calls"
        errSpark = bucketed { Double($0.filter(Analytics.isError).count) / Double($0.count) }

        let sorted = window.filter { $0.state != .tunneled }.map(\.durationMs).sorted()
        p95 = Analytics.percentile(sorted, 0.95)
        latSpark = bucketed { Double(Analytics.percentile($0.map(\.durationMs).sorted(), 0.95)) }

        bytes = window.reduce(0) { $0 + $1.responseBodySize + $1.requestBodySize }
        let imgBytes = window.filter { Analytics.typeBadge($0) == "IMG" }.reduce(0) { $0 + $1.responseBodySize }
        imageShare = bytes > 0 && imgBytes > 0 ? "images \(Int(Double(imgBytes) / Double(bytes) * 100))%" : "response bodies"
        var running = 0.0
        bytesSpark = bucketed { Double($0.reduce(0) { $0 + $1.responseBodySize }) }.map { running += $0; return running }

        let eps = Analytics.endpoints(w).map { DashEndpoint($0, w) }
        failing = eps.filter { $0.errors > 0 }.sorted { ($0.errorRate, $0.calls) > ($1.errorRate, $1.calls) }
        slowest = eps.filter { !$0.tunnel }.sorted { $0.p95 > $1.p95 }
        slowestNote = slowest.first.map { "\($0.path) \(formatMs($0.p95))" } ?? "no calls yet"
        var h: [String: Int] = [:]
        for t in window { h[t.host, default: 0] += 1 }
        hosts = h.map { ($0.key, $0.value) }.sorted { $0.1 > $1.1 }
        latest = Array(all.suffix(6).reversed())
        var apps: [String: Int] = [:]
        for t in window { apps[Analytics.appName(t), default: 0] += 1 }
        mainApp = apps.max { $0.value < $1.value }?.key
    }
}

private struct DashEndpoint: Identifiable {
    let id: String
    let method: String
    let path: String
    let host: String
    let calls: Int
    let errors: Int
    let errorRate: Double
    let p95: UInt64
    let tunnel: Bool
    let lastStatus: UInt16?

    init(_ s: EndpointStat, _ txs: [FfiTransaction]) {
        id = s.id; method = s.method; path = s.path; host = s.host
        calls = s.calls; errors = s.errors; errorRate = s.errorRate; p95 = s.p95; tunnel = s.tunnel
        lastStatus = txs.last { $0.method == s.method && (s.tunnel ? $0.state == .tunneled : Analytics.foldPath($0.path) == s.path) && Analytics.isError($0) }?.status
    }
}
