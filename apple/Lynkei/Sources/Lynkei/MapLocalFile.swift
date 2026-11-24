import LynkeiKit
import SwiftUI

/// Inline file path for a map local rule: type or choose a different file.
struct MapLocalFile: View {
    @Environment(TrafficStore.self) private var store
    let rule: FfiRule
    @State private var path: String
    @State private var error: String?

    init(rule: FfiRule) {
        self.rule = rule
        _path = State(initialValue: rule.file)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Image(systemName: "doc").font(.system(size: 10)).foregroundStyle(Theme.muted)
                TextField("File path", text: $path)
                    .textFieldStyle(.roundedBorder).font(Theme.mono).controlSize(.small)
                    .onSubmit(save)
                Button("Choose…") {
                    if let p = FilePicker.choose() { path = p; save() }
                }
                .controlSize(.small)
            }
            if let error { Text(error).font(.system(size: 11)).foregroundStyle(Theme.s5xx) }
        }
        .frame(width: 400)
    }

    private func save() {
        do {
            try store.setMapLocalFile(rule.id, file: path, status: 0)
            error = nil
        } catch { self.error = "\(error)" }
    }
}
