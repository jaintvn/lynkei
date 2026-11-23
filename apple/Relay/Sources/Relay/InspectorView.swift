import RelayKit
import SwiftUI

/// The right-hand panel: summary, quick actions and the Overview / Request / Response / Timing tabs.
struct InspectorView: View {
    let tx: FfiTransaction?
    @Environment(TrafficStore.self) private var store
    @Environment(Sheets.self) private var sheets
    @State private var tab = ProcessInfo.processInfo.environment["RELAY_TAB"] ?? "Overview"

    /// Overview / Request / Response / Timing, plus a tab for what the call turned out to be.
    private func tabs(for tx: FfiTransaction) -> [String] {
        var t = ["Overview", "Request", "Response", "Timing"]
        if GraphQL.isGraphQL(tx) { t.append("GraphQL") }
        if EventDecoder.vendor(tx) != nil { t.append("Events") }
        if LogStore.shared.hasLogs { t.append("Logs") }
        return t
    }

    var body: some View {
        if let tx {
            VStack(spacing: 0) {
                header(tx)
                tabBar(tabs(for: tx))
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        switch tabs(for: tx).contains(tab) ? tab : "Overview" {
                        case "Request": messageTab(tx, isRequest: true)
                        case "Response": messageTab(tx, isRequest: false)
                        case "Timing": timing(tx)
                        case "GraphQL": GraphQLTab(tx: tx)
                        case "Events": EventsTab(tx: tx)
                        case "Logs": LogsTab(tx: tx)
                        default: overview(tx)
                        }
                    }
                    .padding(.horizontal, 18)
                    .padding(.vertical, 16)
                }
            }
        } else {
            VStack(spacing: 8) {
                Image(systemName: "sidebar.right").font(.system(size: 22)).foregroundStyle(Theme.faint)
                Text("Select a request").foregroundStyle(Theme.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    // MARK: Header and tabs

    private func header(_ tx: FfiTransaction) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                StatusPill(tx: tx, large: true)
                Text(tx.method).font(.system(size: 13, weight: .semibold, design: .monospaced)).foregroundStyle(methodColor(tx.method))
                Text(tx.state == .tunneled ? "\(tx.host):\(tx.port)" : tx.path)
                    .font(.system(size: 13, weight: .medium, design: .monospaced))
                    .lineLimit(1).truncationMode(.middle)
            }
            Text("\(tx.host) · \(Analytics.appName(tx)) · \(Analytics.deviceLabel(tx)) · \(clock(tx.startedAtMs))")
                .font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
            FlowLayout(spacing: 6) {
                Button { store.addQuickRule("breakpoint", for: tx) } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "pause.circle").font(.system(size: 12, weight: .semibold))
                        Text("Pause")
                        Text("⌘B").opacity(0.7).fontWeight(.medium)
                    }
                }
                .buttonStyle(AccentButtonStyle())
                .keyboardShortcut("b", modifiers: .command)
                .help("Hold the next call like this one so you can look at it and change it")
                small("Mock") { sheets.mapLocalFor = tx }
                small("Modify") { sheets.rewriting = RewriteDraft(copying: tx) }
                small("Repeat") { store.repeatNow(tx) }
                small("Copy cURL") { copy(curl(tx)) }
            }
        }
        .padding(.horizontal, 18)
        .padding(.top, 16).padding(.bottom, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    private func small(_ title: String, action: @escaping () -> Void) -> some View {
        Button(title, action: action).buttonStyle(PillButtonStyle()).font(.system(size: 12, weight: .medium))
    }

    private func tabBar(_ tabs: [String]) -> some View {
        let current = tabs.contains(tab) ? tab : "Overview"
        return HStack(spacing: 14) {
            ForEach(tabs, id: \.self) { t in
                Button { tab = t } label: {
                    Text(t)
                        .font(.system(size: 12.5, weight: .medium))
                        .lineLimit(1).fixedSize()
                        .foregroundStyle(current == t ? Theme.fg : Theme.muted)
                        .frame(height: 38)
                        .overlay(alignment: .bottom) { if current == t { Rectangle().fill(Theme.accent).frame(height: 2) } }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer()
        }
        .padding(.horizontal, 18)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    // MARK: Tabs

    @ViewBuilder
    private func overview(_ tx: FfiTransaction) -> some View {
        facts(tx)
        GRPCSummary(tx: tx)
        timing(tx)
        section("Response body") { BodyView(tx: tx, isRequest: false) }
        if Analytics.isError(tx) { failing(tx) }
        if let error = tx.error, tx.state == .failed {
            callout(title: "The call failed", text: error)
        }
        EndpointRules(tx: tx)
        if !tx.websocketMessages.isEmpty {
            section("WebSocket · \(tx.websocketMessages.count) messages") { WebSocketView(messages: tx.websocketMessages) }
        }
    }

    @ViewBuilder
    private func messageTab(_ tx: FfiTransaction, isRequest: Bool) -> some View {
        section("Headers") { HeadersView(headers: isRequest ? tx.requestHeaders : tx.responseHeaders) }
        section(isRequest ? "Request body" : "Response body") { BodyView(tx: tx, isRequest: isRequest) }
    }

    private func facts(_ tx: FfiTransaction) -> some View {
        let items: [(String, String, Color)] = [
            ("Duration", tx.durationMs == 0 ? "—" : formatMs(tx.durationMs), tx.durationMs > 800 ? Theme.s4xx : Theme.fg),
            ("Size", formatBytes(tx.responseBodySize), Theme.fg),
            ("Protocol", protocolLabel(tx), Theme.fg),
            ("TLS", tx.decrypted ? "Decrypted" : (tx.scheme == "https" ? "Tunnel" : "None"), Theme.fg),
            ("Remote", "\(tx.host):\(tx.port)", Theme.fg),
            ("API", Analytics.apiVersion(tx.path) ?? "—", Theme.fg),
        ]
        return LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 1), count: 3), spacing: 1) {
            ForEach(items, id: \.0) { k, v, c in
                VStack(alignment: .leading, spacing: 3) {
                    Text(k).font(.system(size: 11)).foregroundStyle(Theme.faint)
                    Text(v).font(.system(size: 12.5, weight: .medium, design: .monospaced)).foregroundStyle(c).lineLimit(1).truncationMode(.middle)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.vertical, 10)
                .background(Theme.panel)
            }
        }
        .background(Theme.line)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
    }

    /// Relay measures the whole exchange; the split into DNS / connect / TLS is not captured yet.
    private func timing(_ tx: FfiTransaction) -> some View {
        section("Timing") {
            VStack(spacing: 8) {
                timingRow("Total", ms: tx.durationMs, of: max(tx.durationMs, 1), color: tx.durationMs > 800 ? Theme.s4xx : Theme.accent)
                timingRow("Request", ms: nil, size: tx.requestBodySize)
                timingRow("Response", ms: nil, size: tx.responseBodySize)
            }
        }
    }

    private func timingRow(_ label: String, ms: UInt64?, of total: UInt64 = 1, color: Color = Theme.s3xx, size: UInt64? = nil) -> some View {
        HStack(spacing: 10) {
            Text(label).font(.system(size: 11.5)).foregroundStyle(Theme.muted).frame(width: 64, alignment: .leading)
            if let ms {
                MeterBar(fraction: Double(ms) / Double(total), color: color, height: 8)
                Text(ms == 0 ? "—" : formatMs(ms)).font(.system(size: 11.5, design: .monospaced)).frame(width: 64, alignment: .trailing)
            } else {
                Spacer()
                Text(size.map(formatBytes) ?? "—").font(.system(size: 11.5, design: .monospaced)).frame(width: 64, alignment: .trailing)
            }
        }
    }

    private func failing(_ tx: FfiTransaction) -> some View {
        let key = Analytics.foldPath(tx.path)
        let recent = store.transactions.filter { $0.host == tx.host && $0.method == tx.method && Analytics.foldPath($0.path) == key }.suffix(6)
        let bad = recent.filter(Analytics.isError).count
        return callout(
            title: "Failing \(bad) of the last \(recent.count) calls",
            text: tx.error ?? "\(tx.method) \(key) on \(tx.host) answered \(tx.status.map(String.init) ?? "with an error").")
    }

    private func callout(title: String, text: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.circle").foregroundStyle(Theme.s5xx).font(.system(size: 15))
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.system(size: 12, weight: .semibold))
                Text(text).font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.s5xx.opacity(0.10)))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.s5xx.opacity(0.30)))
    }

    private func clock(_ ms: UInt64) -> String {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    /// "HTTP/2", or "HTTP/1.1 → HTTP/2" when the app and the server legs differ.
    private func protocolLabel(_ tx: FfiTransaction) -> String {
        guard let upstream = tx.upstreamHttpVersion, upstream != tx.httpVersion else { return tx.httpVersion }
        return "\(tx.httpVersion) → \(upstream)"
    }

    private func section<C: View>(_ title: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.system(size: 12, weight: .semibold))
            content()
        }
    }

    private func copy(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }

    private func curl(_ tx: FfiTransaction) -> String {
        var parts = ["curl", "-X", tx.method, "'\(tx.url)'"]
        for h in tx.requestHeaders where !["host", "content-length"].contains(h.name.lowercased()) {
            parts.append("-H '\(h.name): \(h.value.replacingOccurrences(of: "'", with: "'\\''"))'")
        }
        if let body = tx.requestBodyPreview { parts.append("--data-raw '\(body.replacingOccurrences(of: "'", with: "'\\''"))'") }
        return parts.joined(separator: " ")
    }
}


struct HeadersView: View {
    let headers: [FfiHeader]
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if headers.isEmpty {
                Text("None").foregroundStyle(Theme.faint).padding(10)
            }
            ForEach(Array(headers.enumerated()), id: \.offset) { _, h in
                HStack(alignment: .top, spacing: 10) {
                    Text(h.name).foregroundStyle(Theme.key).frame(width: 150, alignment: .leading)
                    Text(h.value).foregroundStyle(Theme.fg).textSelection(.enabled)
                }
                .font(.system(size: 11.5, design: .monospaced))
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
    }
}

/// Frames of an upgraded WebSocket connection: direction, type, size, text preview.
struct WebSocketView: View {
    let messages: [FfiWsMessage]
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(messages.prefix(300).enumerated()), id: \.offset) { _, m in
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: m.sent ? "arrow.up" : "arrow.down")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(m.sent ? Theme.accent : Theme.s2xx)
                        .frame(width: 12)
                    Text(m.kind).font(Theme.monoSmall).foregroundStyle(Theme.muted).frame(width: 64, alignment: .leading)
                    Text(m.preview ?? "\(formatBytes(m.size))")
                        .font(.system(size: 11.5, design: .monospaced))
                        .lineLimit(3)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Text("\(m.atMs) ms").font(.system(size: 10.5, design: .monospaced)).foregroundStyle(Theme.faint)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
    }
}

/// The rules that fit this endpoint, each with its on/off switch.
struct EndpointRules: View {
    @Environment(TrafficStore.self) private var store
    let tx: FfiTransaction

    var body: some View {
        // `store.rules` is read so the list refreshes when a rule is switched.
        let matching = store.rules.isEmpty ? [] : store.rulesFor(tx)
        if !matching.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("RULES FOR THIS ENDPOINT").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                ForEach(matching, id: \.id) { r in
                    HStack(spacing: 8) {
                        Image(systemName: RuleBadge.symbol(r.kind))
                            .foregroundStyle(r.enabled ? Theme.paused : Theme.faint)
                            .frame(width: 16)
                        VStack(alignment: .leading, spacing: 1) {
                            Text("\(r.kind.capitalized) · \(r.id)").font(.system(size: 12.5))
                            Text(r.enabled ? ruleHelp(r.kind) : "Off — not applied")
                                .font(.system(size: 11)).foregroundStyle(Theme.muted)
                        }
                        Spacer()
                        Toggle("", isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                            .labelsHidden().toggleStyle(.switch).controlSize(.small)
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line2))
        }
    }

    private func ruleHelp(_ kind: String) -> String {
        switch kind {
        case "breakpoint": return "On — the next matching call pauses so you can edit it"
        case "block": return "On — matching calls are refused"
        case "map local": return "On — answered from a local file"
        case "map remote": return "On — sent to another server"
        case "rewrite": return "On — edited on the way through"
        case "throttle": return "On — slowed or dropped"
        case "dns spoof": return "On — this host is looked up at another address"
        default: return "On"
        }
    }
}

/// Small marker on a row in the list when rules changed or held that call.
enum RuleBadge {
    static func symbol(_ kind: String) -> String {
        switch kind {
        case "breakpoint": return "pause.circle.fill"
        case "block": return "nosign"
        case "map local": return "doc.fill"
        case "map remote": return "arrow.triangle.branch"
        case "rewrite": return "pencil.circle.fill"
        case "throttle": return "tortoise.fill"
        case "dns spoof": return "network"
        default: return "slider.horizontal.3"
        }
    }
}
