import RelayKit
import SwiftUI

/// Endpoints (canvas board 4): calls folded by method and path, with error rate, latency and a trend.
struct EndpointsView: View {
    @Environment(TrafficStore.self) private var store
    @Binding var screen: Screen
    @State private var sort = "Most failing"
    @State private var version: String?

    private let sorts = ["Most failing", "Slowest", "Most called", "Largest"]
    private let cols: [CGFloat] = [70, 80, 120, 80, 80, 90, 100, 150]

    var body: some View {
        VStack(spacing: 0) {
            AppHeader(screen: $screen) { Spacer(minLength: 0) }
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                let all = Analytics.endpoints(store.transactions).filter { s in
                    guard let v = version else { return true }
                    return s.path.hasPrefix("/\(v)/") || s.path == "/\(v)"
                }
                let rows = sorted(all)
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        titleRow(all: Analytics.endpoints(store.transactions))
                        table(rows)
                    }
                    .padding(.horizontal, 24).padding(.vertical, 22)
                    .frame(maxWidth: 1440).frame(maxWidth: .infinity)
                }
            }
        }
    }

    private func sorted(_ list: [EndpointStat]) -> [EndpointStat] {
        switch sort {
        case "Slowest": return list.sorted { $0.p95 > $1.p95 }
        case "Most called": return list.sorted { $0.calls > $1.calls }
        case "Largest": return list.sorted { $0.avgSize > $1.avgSize }
        default: return list.sorted { ($0.errorRate, $0.errors, $0.calls) > ($1.errorRate, $1.errors, $1.calls) }
        }
    }

    private func titleRow(all: [EndpointStat]) -> some View {
        let versions = Array(Set(all.compactMap { s in Analytics.apiVersion(s.path) })).sorted()
        return HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Endpoints").font(.system(size: 22, weight: .semibold)).tracking(-0.4)
                Text("IDs are folded automatically: \(Text("/users/8231").font(.system(size: 13, design: .monospaced)).foregroundStyle(Theme.fg)) → \(Text("/users/{id}").font(.system(size: 13, design: .monospaced)).foregroundStyle(Theme.fg))")
                    .foregroundStyle(Theme.muted)
            }
            Spacer()
            PillGroup(items: sorts, title: { $0 }, selection: $sort)
            Menu {
                Button("All versions") { version = nil }
                if !versions.isEmpty { Divider() }
                ForEach(versions, id: \.self) { v in Button(v) { version = v } }
            } label: {
                DashedChip(label: "API version: \(version ?? "all")", chevron: false).frame(height: 30)
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        }
    }

    private func table(_ rows: [EndpointStat]) -> some View {
        let now = Analytics.nowMs()
        return VStack(spacing: 0) {
            do {
                VStack(spacing: 0) {
                    row {
                        Text("METHOD").frame(width: 70, alignment: .leading)
                        Text("ENDPOINT").frame(minWidth: 240, maxWidth: .infinity, alignment: .leading)
                        Text("CALLS").frame(width: 80, alignment: .trailing)
                        Text("ERRORS").frame(width: 120, alignment: .leading)
                        Text("P50").frame(width: 80, alignment: .trailing)
                        Text("P95").frame(width: 80, alignment: .trailing)
                        Text("AVG SIZE").frame(width: 90, alignment: .trailing)
                        Text("LAST SEEN").frame(width: 100, alignment: .trailing)
                        Text("LAST 15 MIN").frame(width: 150, alignment: .leading)
                    }
                    .font(.system(size: 11, weight: .semibold)).tracking(0.4).foregroundStyle(Theme.faint)
                    .frame(height: 36)
                    if rows.isEmpty {
                        Text("No calls captured yet.").font(.system(size: 13)).foregroundStyle(Theme.muted).padding(24)
                    }
                    ForEach(rows) { e in
                        let ec = e.errorRate >= 0.5 ? Theme.s5xx : e.errors > 0 ? Theme.s4xx : Theme.s2xx
                        row {
                            Text(e.method).font(.system(size: 11.5, weight: .semibold, design: .monospaced)).foregroundStyle(methodColor(e.method)).frame(width: 70, alignment: .leading)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(e.path).font(.system(size: 12.5, design: .monospaced)).lineLimit(1)
                                Text(e.host).font(.system(size: 11)).foregroundStyle(Theme.muted).lineLimit(1)
                            }
                            .frame(minWidth: 240, maxWidth: .infinity, alignment: .leading)
                            Text("\(e.calls)").font(.system(size: 13, design: .monospaced)).frame(width: 80, alignment: .trailing)
                            HStack(spacing: 8) {
                                MeterBar(fraction: max(e.errorRate, 0.02), color: ec, height: 5)
                                Text("\(Int((e.errorRate * 100).rounded()))%").font(.system(size: 12, design: .monospaced)).foregroundStyle(ec).frame(width: 38, alignment: .trailing)
                            }
                            .frame(width: 120)
                            Text(e.tunnel ? "—" : formatMs(e.p50)).font(.system(size: 13, design: .monospaced)).foregroundStyle(Theme.muted).frame(width: 80, alignment: .trailing)
                            Text(e.tunnel ? "—" : formatMs(e.p95)).font(.system(size: 13, design: .monospaced)).foregroundStyle(e.p95 > 800 ? Theme.s4xx : Theme.fg).frame(width: 80, alignment: .trailing)
                            Text(e.avgSize == 0 ? "—" : formatBytes(e.avgSize)).font(.system(size: 13, design: .monospaced)).foregroundStyle(Theme.muted).frame(width: 90, alignment: .trailing)
                            Text(Analytics.ago(e.lastSeenMs, now: now)).font(.system(size: 12)).foregroundStyle(Theme.muted).frame(width: 100, alignment: .trailing)
                            Sparkline(values: e.buckets, color: e.errors > 0 ? ec : Theme.accent).frame(width: 150, height: 24)
                        }
                        .frame(height: 44)
                    }
                }
                .frame(minWidth: 1100, maxWidth: .infinity)
            }
        }
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
        .clipShape(RoundedRectangle(cornerRadius: 14))
    }

    private func row<C: View>(@ViewBuilder _ content: () -> C) -> some View {
        HStack(spacing: 14) { content() }
            .padding(.horizontal, 18)
            .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }
}
