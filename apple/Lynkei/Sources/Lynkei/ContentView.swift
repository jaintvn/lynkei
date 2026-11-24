import LynkeiKit
import SwiftUI

/// The four screens of the app, as in the design canvas: Dashboard, Traffic, Endpoints, Rules.
enum Screen: String, CaseIterable, Hashable {
    case dashboard = "Dashboard", traffic = "Traffic", endpoints = "Endpoints", rules = "Rules"
}

/// Sheets that more than one screen can open (rewrite, compose, map local…).
@MainActor
@Observable
final class Sheets {
    var showConnect = false
    var mapLocalFor: FfiTransaction?
    var composing: ComposeDraft?
    var dnsFor: FfiTransaction?
    var rewriting: RewriteDraft?
    /// Rule to select when the Rules screen opens.
    var ruleToOpen: String?
}

struct ContentView: View {
    @Environment(TrafficStore.self) private var store
    @State private var screen: Screen = Screen(rawValue: ProcessInfo.processInfo.environment["LYNKEI_SCREEN"] ?? "") ?? .traffic
    @State private var sheets = Sheets()

    var body: some View {
        @Bindable var sheets = sheets
        Group {
            if store.paused.first != nil {
                BreakpointScreen()
            } else {
                switch screen {
                case .dashboard: DashboardView(screen: $screen)
                case .traffic: TrafficScreen(screen: $screen)
                case .endpoints: EndpointsView(screen: $screen)
                case .rules: RulesScreen(screen: $screen)
                }
            }
        }
        .background(Theme.bg)
        .foregroundStyle(Theme.fg)
        .environment(sheets)
        .task {
            if ProcessInfo.processInfo.environment["LYNKEI_OPEN_CONNECT"] != nil {
                try? await Task.sleep(for: .seconds(2))
                sheets.showConnect = true
            }
        }
        .task {
            // For demos and screenshots: open the Rewrite editor on the newest call that has a JSON response.
            guard ProcessInfo.processInfo.environment["LYNKEI_OPEN_REWRITE"] != nil else { return }
            try? await Task.sleep(for: .seconds(4))
            if let tx = store.transactions.last(where: { $0.responseBodyPreview?.hasPrefix("{") == true }) {
                var d = RewriteDraft(copying: tx)
                if let f = jsonLeaves(tx.responseBodyPreview).first(where: { $0.json.first?.isNumber == true }) {
                    var step = StepDraft(kind: "set_json"); step.path = f.path; step.value = "999"
                    d.sides[1].steps.append(step)
                }
                sheets.rewriting = d
            }
        }
        .sheet(isPresented: $sheets.showConnect) { ConnectView() }
        .sheet(item: $sheets.mapLocalFor) { MapLocalView(tx: $0) }
        .sheet(item: $sheets.composing) { ComposeView(draft: $0) }
        .sheet(item: $sheets.dnsFor) { DnsSpoofView(tx: $0) }
        .sheet(item: $sheets.rewriting) { RewriteView(draft: $0) }
    }
}

// MARK: - Header shared by the four screens

struct AppHeader<Trailing: View>: View {
    @Binding var screen: Screen
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 14) {
            HStack(spacing: 8) {
                LynkeiMark(size: 22)
                Text("Lynkei").font(.system(size: 14, weight: .semibold))
            }
            .padding(.leading, -2)  // the traffic lights sit in the strip above this row; line the mark's ink up with the sidebar text
            PillGroup(items: Screen.allCases, title: { $0.rawValue }, selection: $screen, height: 24, fontSize: 12.5)
            trailing
        }
        .padding(.horizontal, 16)
        .frame(height: 52)
        .background(Theme.panel)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }
}

// MARK: - Shared row pieces

struct StatusPill: View {
    let tx: FfiTransaction
    var large = false
    var body: some View {
        Text(label)
            .font(.system(size: large ? 12 : 11, weight: large ? .bold : .semibold, design: .monospaced))
            .foregroundStyle(color)
            .padding(.horizontal, large ? 8 : 7)
            .padding(.vertical, large ? 3 : 2)
            .background(RoundedRectangle(cornerRadius: large ? 6 : 5).fill(color.opacity(large ? 0.16 : 0.14)))
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
            MeterBar(fraction: Double(ms) / 2000, color: ms > 800 ? Theme.s4xx : Theme.accent, height: 4)
            Text(ms == 0 ? "—" : formatMs(ms))
                .font(.system(size: 11.5, design: .monospaced))
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
            Text("iOS Simulator: use Connect device, or set this Mac's HTTP/HTTPS proxy to 127.0.0.1:\(port.map(String.init) ?? "8888").")
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
