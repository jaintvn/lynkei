import LynkeiKit
import SwiftUI

/// Asks where a host name should point instead (DNS spoofing).
struct DnsSpoofView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let tx: FfiTransaction

    @State private var host = ""
    @State private var target = "127.0.0.1"
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Send a host to another address").font(.system(size: 16, weight: .semibold))
            Text("Lynkei connects to this address instead of the real one. The app still talks to the original host name, so Host headers and certificates are unchanged. Like an /etc/hosts entry, but only for traffic through Lynkei.")
                .font(.system(size: 12)).foregroundStyle(Theme.muted)
            HStack(spacing: 8) {
                TextField("Host", text: $host).textFieldStyle(.roundedBorder).font(Theme.mono)
                Image(systemName: "arrow.right").foregroundStyle(Theme.muted)
                TextField("127.0.0.1 or staging.internal", text: $target)
                    .textFieldStyle(.roundedBorder).font(Theme.mono).onSubmit(save)
            }
            Text("Subdomains of the host follow too. Existing connections to the old address end within a minute or so.")
                .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Add rule") { save() }
                    .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                    .disabled(host.trimmingCharacters(in: .whitespaces).isEmpty || target.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(18)
        .frame(width: 560)
        .background(Theme.panel)
        .onAppear { host = tx.host }
    }

    private func save() {
        do {
            try store.addDnsSpoof(host: host, to: target)
            dismiss()
        } catch { self.error = "\(error)" }
    }
}
