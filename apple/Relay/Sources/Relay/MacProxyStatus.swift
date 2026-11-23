import CFNetwork
import Foundation
import Observation

/// What the Mac's system proxy is right now (what simulators and Mac apps use), read live from macOS,
/// so the Connect screen can say when something (a VPN, a company profile, the user) turned it off.
@MainActor
@Observable
final class MacProxyStatus {
    static let shared = MacProxyStatus()

    enum State: Equatable {
        case unknown
        case relay(port: Int)
        case off
        case other(String)
        /// A proxy auto-config script is on, which can send traffic around Relay.
        case pac(String)
    }

    private(set) var state: State = .unknown

    func refresh(relayPort: UInt16?) {
        guard let s = CFNetworkCopySystemProxySettings()?.takeRetainedValue() as? [String: Any] else { state = .unknown; return }
        func int(_ k: String) -> Int { (s[k] as? NSNumber)?.intValue ?? 0 }
        let httpOn = int("HTTPEnable") == 1, httpsOn = int("HTTPSEnable") == 1
        let host = s["HTTPProxy"] as? String ?? "", port = int("HTTPPort")
        if httpOn, httpsOn, host == "127.0.0.1" || host == "localhost", let relayPort, port == Int(relayPort) {
            state = .relay(port: port)
        } else if int("ProxyAutoConfigEnable") == 1 {
            state = .pac(s["ProxyAutoConfigURLString"] as? String ?? "")
        } else if httpOn || httpsOn {
            state = .other("\(host):\(port)")
        } else {
            state = .off
        }
    }

    var isRelay: Bool { if case .relay = state { return true } else { return false } }
}
