import AppKit
import RelayKit
import SwiftUI

/// Asks which local file should answer a request.
struct MapLocalView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let tx: FfiTransaction

    @State private var path = ""
    @State private var status = "200"
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Map local").font(.system(size: 16, weight: .semibold))
            Text("\(tx.method) \(tx.host)\(tx.path.split(separator: "?").first.map { String($0) } ?? "")")
                .font(Theme.mono).foregroundStyle(Theme.muted).lineLimit(1).truncationMode(.middle)
            Text("Calls to this endpoint get the contents of this file instead of the server's answer.")
                .font(.system(size: 12)).foregroundStyle(Theme.muted)

            HStack(spacing: 8) {
                TextField("Path to a file, e.g. ~/Desktop/products.json", text: $path)
                    .textFieldStyle(.roundedBorder).font(Theme.mono)
                    .onSubmit(save)
                Button("Choose…") { if let p = FilePicker.choose() { path = p } }
            }
            HStack(spacing: 8) {
                Text("Status").font(.system(size: 12)).foregroundStyle(Theme.muted)
                TextField("200", text: $status).textFieldStyle(.roundedBorder).frame(width: 70)
                Spacer()
                if tx.responseBodyPreview != nil {
                    Button("Use this response instead") {
                        store.addQuickRule("map-local", for: tx)
                        dismiss()
                    }
                    .help("Saves the recorded response under mocks/ and uses that")
                }
            }
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Add rule") { save() }
                    .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                    .disabled(path.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(18)
        .frame(width: 560)
        .background(Theme.panel)
    }

    private func save() {
        do {
            try store.addMapLocal(tx, file: path, status: UInt16(status) ?? 200)
            dismiss()
        } catch { self.error = "\(error)" }
    }
}

enum FilePicker {
    /// The system file chooser; returns the picked file's path.
    @MainActor static func choose() -> String? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose the file to answer with"
        return panel.runModal() == .OK ? panel.url?.path : nil
    }
}
