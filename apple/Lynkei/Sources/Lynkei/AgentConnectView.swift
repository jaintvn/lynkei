import AppKit
import SwiftUI

/// Lynkei's MCP server for AI agents: a fixed local address that any agent can use, always the same one.
/// It is the bundled `lynkei-mcp` helper in HTTP mode, talking to this app's own engine.
@MainActor
@Observable
final class AgentAccess {
    static let shared = AgentAccess()

    enum State: Equatable { case off, starting, running, failed(String) }

    var state: State = .off
    var enabled: Bool { didSet { UserDefaults.standard.set(enabled, forKey: "agentAccess"); apply() } }
    var port: Int { didSet { UserDefaults.standard.set(port, forKey: "agentPort"); if enabled { restart() } } }

    private var socket: String?
    private var process: Process?
    private var stderr = Pipe()

    init() {
        enabled = UserDefaults.standard.object(forKey: "agentAccess") as? Bool ?? true
        let p = UserDefaults.standard.integer(forKey: "agentPort")
        port = p == 0 ? 8890 : p
    }

    var url: String { "http://127.0.0.1:\(port)/mcp" }
    var configJSON: String { "{\n  \"mcpServers\": {\n    \"lynkei\": {\n      \"url\": \"\(url)\"\n    }\n  }\n}" }
    var bridgeCommand: String { "npx -y mcp-remote \(url)" }

    /// Called once the app's engine can answer the control API.
    func engineReady(socket: String) {
        self.socket = socket
        apply()
    }

    func stop() {
        process?.terminationHandler = nil
        process?.terminate()
        process = nil
    }

    private func restart() { stop(); apply() }

    private func apply() {
        guard enabled else { stop(); state = .off; return }
        guard process == nil, let socket else { return }
        let helper = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/lynkei-mcp")
        guard FileManager.default.isExecutableFile(atPath: helper.path) else {
            state = .failed("This build has no agent helper. Build the app with scripts/make-mac-app.sh.")
            return
        }
        let p = Process()
        p.executableURL = helper
        p.arguments = ["--http", "127.0.0.1:\(port)", "--socket", socket]
        stderr = Pipe()
        p.standardError = stderr
        p.standardOutput = FileHandle.nullDevice
        let err = stderr
        p.terminationHandler = { [weak self] proc in
            let text = String(data: err.fileHandleForReading.availableData, encoding: .utf8) ?? ""
            Task { @MainActor in
                guard let self, self.process === proc else { return }
                self.process = nil
                let busy = text.contains("in use")
                self.state = .failed(busy ? "Port \(self.port) is used by another program. Pick another port below." : "The agent server stopped. \(text.prefix(160))")
            }
        }
        do {
            try p.run()
            process = p
            state = .starting
            // The helper prints nothing on success; confirm by asking it.
            Task { @MainActor in
                for _ in 0..<20 {
                    try? await Task.sleep(for: .milliseconds(250))
                    guard self.process === p else { return }
                    if await Self.responds(port: self.port) { self.state = .running; return }
                }
            }
        } catch {
            state = .failed("\(error.localizedDescription)")
        }
    }

    private static func responds(port: Int) async -> Bool {
        guard let u = URL(string: "http://127.0.0.1:\(port)/health") else { return false }
        return (try? await URLSession.shared.data(from: u)) != nil
    }
}

struct AgentConnectView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var access = AgentAccess.shared
    @State private var copied: String?
    @State private var portText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Connect an AI agent").font(.system(size: 18, weight: .semibold))
                Text("Lynkei runs an MCP server at a fixed local address. Add it to any AI agent once and it keeps working, whatever network you are on. The agent sees exactly what you see here: live traffic, rules, devices and held requests.")
                    .font(.system(size: 12.5)).foregroundStyle(Theme.muted)
            }

            HStack(spacing: 10) {
                statusDot
                VStack(alignment: .leading, spacing: 2) {
                    Text(statusTitle).font(.system(size: 13, weight: .semibold))
                    if case .failed(let why) = access.state {
                        Text(why).font(.system(size: 11.5)).foregroundStyle(Theme.s5xx)
                    } else {
                        Text("Only this Mac can reach it. Agents work while Lynkei is open.").font(.system(size: 11.5)).foregroundStyle(Theme.faint)
                    }
                }
                Spacer()
                Toggle("", isOn: $access.enabled).toggleStyle(.switch).labelsHidden()
            }
            .padding(12)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))

            step(1, "Add this server to your agent",
                 "Open your agent's MCP settings and add a server with this URL (transport: HTTP). Or paste this JSON.") {
                copyRow(access.url, key: "url")
                copyRow(access.configJSON, key: "json", multiline: true)
            }
            step(2, "Restart the agent", "Agents read their MCP list at launch.") { EmptyView() }
            step(3, "Ask it something",
                 "“Use Lynkei to connect my iPhone simulator and tell me which requests are failing.”") { EmptyView() }

            DisclosureGroup {
                VStack(alignment: .leading, spacing: 6) {
                    Text("If your agent only supports local (stdio) servers, bridge it with:")
                        .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
                    copyRow(access.bridgeCommand, key: "bridge")
                    Divider().padding(.vertical, 2)
                    HStack {
                        Text("Port").font(.system(size: 12))
                        TextField("8890", text: $portText).textFieldStyle(.roundedBorder).frame(width: 80)
                            .onSubmit { if let p = Int(portText), (1024...65535).contains(p) { access.port = p } }
                        Text("Change it only if another program uses \(access.port). Update your agent after.")
                            .font(.system(size: 11.5)).foregroundStyle(Theme.faint)
                    }
                }.padding(.top, 6)
            } label: { Text("More").font(.system(size: 12.5, weight: .medium)) }

            HStack {
                Text("Secrets such as Authorization headers are hidden from agents unless they ask.")
                    .font(.system(size: 11)).foregroundStyle(Theme.faint)
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 600)
        .background(Theme.panel)
        .onAppear { portText = String(access.port) }
    }

    private var statusTitle: String {
        switch access.state {
        case .off: "Agent access is off"
        case .starting: "Starting…"
        case .running: "Running at \(access.url)"
        case .failed: "Not running"
        }
    }

    private var statusDot: some View {
        Circle().fill(access.state == .running ? Theme.s2xx : (access.state == .off ? Theme.faint : Theme.s5xx)).frame(width: 9, height: 9)
    }

    private func step<Content: View>(_ n: Int, _ title: String, _ detail: String, @ViewBuilder content: () -> Content) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(n)").font(.system(size: 12, weight: .bold)).foregroundStyle(Theme.accent)
                .frame(width: 22, height: 22).background(Circle().fill(Theme.selection))
            VStack(alignment: .leading, spacing: 6) {
                Text(title).font(.system(size: 13, weight: .semibold))
                Text(detail).font(.system(size: 12)).foregroundStyle(Theme.muted)
                content()
            }
        }
    }

    private func copyRow(_ text: String, key: String, multiline: Bool = false) -> some View {
        HStack(alignment: .top) {
            Text(text).font(Theme.mono).textSelection(.enabled)
                .padding(10).frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.bg))
            Button(copied == key ? "Copied" : "Copy") {
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string); copied = key
            }.buttonStyle(.bordered)
        }
    }
}
