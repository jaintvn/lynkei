import SwiftUI

/// "Acme Shop" — a tiny stand-in for a real iOS app. It makes a realistic mix of API calls so you
/// can watch Relay track them: list, detail, add to cart (JSON POST), a failing checkout (500),
/// an expired token (401), a slow call and an image. Nothing here knows about Relay, except the
/// optional `-relayProxy host:port` launch argument (see `makeSession`), which lets the Simulator
/// demo run without changing the Mac's system proxy.
struct Call: Identifiable {
    let id = UUID()
    let label: String
    let method: String
    let url: String
    var body: String? = nil
}

let calls: [Call] = [
    Call(label: "Browse shoes", method: "GET", url: "https://httpbin.org/get?category=shoes&page=2"),
    Call(label: "Product list", method: "GET", url: "https://httpbin.org/json"),
    Call(label: "Add to cart", method: "POST", url: "https://httpbin.org/post", body: #"{"sku":"p_8231","qty":1,"price":129.0}"#),
    Call(label: "Checkout (fails)", method: "POST", url: "https://httpbin.org/status/500", body: #"{"cart":{"items":3,"total":248.9}}"#),
    Call(label: "Refresh token (401)", method: "POST", url: "https://httpbin.org/status/401", body: #"{"refresh":"expired"}"#),
    Call(label: "Recommendations (slow)", method: "GET", url: "https://httpbin.org/delay/2"),
    Call(label: "Product image", method: "GET", url: "https://httpbin.org/image/png"),
    Call(label: "Tip of the day", method: "GET", url: "https://api.github.com/zen"),
]

/// A normal session, or one pointed at a proxy when launched with `-relayProxy 127.0.0.1:8888`.
func makeSession() -> URLSession {
    let config = URLSessionConfiguration.default
    let args = ProcessInfo.processInfo.arguments
    if let i = args.firstIndex(of: "-relayProxy"), i + 1 < args.count {
        let parts = args[i + 1].split(separator: ":")
        if parts.count == 2, let port = Int(parts[1]) {
            config.connectionProxyDictionary = [
                "HTTPEnable": 1, "HTTPProxy": String(parts[0]), "HTTPPort": port,
                "HTTPSEnable": 1, "HTTPSProxy": String(parts[0]), "HTTPSPort": port,
            ]
        }
    }
    return URLSession(configuration: config)
}

struct ContentView: View {
    @State private var log: [String] = []
    @State private var running = false
    private let session = makeSession()

    private let brand = Color(red: 1.0, green: 0.32, blue: 0.47)
    private let ink = Color(red: 0.16, green: 0.11, blue: 0.08)

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Acme Shop").font(.system(size: 30, weight: .bold)).foregroundStyle(brand)
                Text("v3.4.1 · demo app for Relay").foregroundStyle(ink.opacity(0.6))
            }
            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 8) {
                ForEach(calls) { call in
                    Button(call.label) { Task { await run(call) } }
                        .font(.system(size: 13, weight: .semibold))
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .foregroundStyle(.white)
                        .background(RoundedRectangle(cornerRadius: 14).fill(brand))
                }
            }
            Button("Run every call") { Task { await runAll() } }
                .font(.system(size: 16, weight: .semibold))
                .frame(maxWidth: .infinity, minHeight: 48)
                .foregroundStyle(.white)
                .background(RoundedRectangle(cornerRadius: 14).fill(ink))
                .disabled(running)
            ScrollView {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(Array(log.enumerated()), id: \.offset) { _, line in
                        Text(line).font(.system(size: 11, design: .monospaced)).foregroundStyle(ink)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
            }
            .background(RoundedRectangle(cornerRadius: 14).fill(.white))
        }
        .padding(20)
        .background(Color(red: 1.0, green: 0.97, blue: 0.95).ignoresSafeArea())
        .task {
            if ProcessInfo.processInfo.arguments.contains("-autorun") { await runAll() }
        }
    }

    private func runAll() async {
        running = true
        for call in calls {
            await run(call)
            try? await Task.sleep(nanoseconds: 400_000_000)
        }
        log.append("— done —")
        running = false
    }

    private func run(_ call: Call) async {
        let started = Date()
        var request = URLRequest(url: URL(string: call.url)!)
        request.httpMethod = call.method
        request.timeoutInterval = 30
        request.setValue("AcmeShop/3.4.1 (iOS)", forHTTPHeaderField: "User-Agent")
        request.setValue("3.4.1", forHTTPHeaderField: "X-App-Version")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body = call.body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = Data(body.utf8)
        }
        do {
            let (data, response) = try await session.data(for: request)
            let code = (response as? HTTPURLResponse)?.statusCode ?? 0
            let ms = Int(Date().timeIntervalSince(started) * 1000)
            log.append("\(code < 400 ? "✓" : "✗") \(code)  \(call.label)  ·  \(data.count) B  ·  \(ms) ms")
        } catch {
            log.append("✗ ERR  \(call.label)  ·  \((error as NSError).localizedDescription)")
        }
    }
}
