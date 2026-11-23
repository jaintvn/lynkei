import RelayKit
import SwiftUI

/// Inspector tabs that appear only for calls Relay can read further: GraphQL, gRPC, analytics events.

struct GraphQLTab: View {
    let tx: FfiTransaction

    var body: some View {
        let ops = GraphQL.operations(tx)
        VStack(alignment: .leading, spacing: 16) {
            if ops.count > 1 {
                Text("\(ops.count) operations in one request (batched)").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            ForEach(ops) { op in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        Text(op.kind.uppercased())
                            .font(.system(size: 10, weight: .bold)).foregroundStyle(op.kind == "mutation" ? Theme.s4xx : Theme.accent)
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .background(RoundedRectangle(cornerRadius: 4).fill((op.kind == "mutation" ? Theme.s4xx : Theme.accent).opacity(0.14)))
                        Text(op.name ?? "anonymous").font(.system(size: 14, weight: .semibold, design: .monospaced))
                    }
                    if let q = op.query { block("Query", q) }
                    if let h = op.persistedHash { block("Persisted query", "sha256: \(h)") }
                    if let v = op.variables { block("Variables", v) }
                }
            }
            if ops.isEmpty { Text("No GraphQL operation found in this call.").foregroundStyle(Theme.faint) }
        }
    }

    private func block(_ title: String, _ text: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.system(size: 11)).foregroundStyle(Theme.faint)
            Text(text)
                .font(.system(size: 11.5, design: .monospaced)).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
        }
    }
}

/// What a gRPC call is: service, method and result. The messages themselves are in Request / Response.
struct GRPCSummary: View {
    let tx: FfiTransaction

    var body: some View {
        if let m = GRPC.method(tx) {
            VStack(alignment: .leading, spacing: 8) {
                Text("gRPC call").font(.system(size: 12, weight: .semibold))
                VStack(alignment: .leading, spacing: 6) {
                    row("Service", m.service)
                    row("Method", m.method)
                    row("Result", GRPC.result(tx) ?? (tx.status == 200 ? "no status seen (trailers are not captured)" : "HTTP \(tx.status.map(String.init) ?? "—")"))
                    Text("Messages are decoded without a .proto file, so fields show as numbers: {\"1\": …}.")
                        .font(.system(size: 11)).foregroundStyle(Theme.faint)
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            }
        }
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .top) {
            Text(k).font(.system(size: 11.5)).foregroundStyle(Theme.faint).frame(width: 60, alignment: .leading)
            Text(v).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
        }
    }
}

struct EventsTab: View {
    let tx: FfiTransaction

    var body: some View {
        let events = EventDecoder.events(tx)
        VStack(alignment: .leading, spacing: 12) {
            if let v = events.first?.vendor {
                Text("\(events.count) event\(events.count == 1 ? "" : "s") sent to \(v)").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            ForEach(events) { e in
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text(e.name).font(.system(size: 13, weight: .semibold, design: .monospaced))
                        Spacer()
                        if let u = e.user { Text(u).font(.system(size: 11)).foregroundStyle(Theme.faint).lineLimit(1).truncationMode(.middle) }
                    }
                    if e.properties.isEmpty {
                        Text("No properties").font(.system(size: 11.5)).foregroundStyle(Theme.faint)
                    }
                    ForEach(Array(e.properties.enumerated()), id: \.offset) { _, p in
                        HStack(alignment: .top, spacing: 10) {
                            Text(p.0).foregroundStyle(Theme.key).frame(width: 130, alignment: .leading)
                            Text(p.1).foregroundStyle(Theme.fg).textSelection(.enabled)
                        }
                        .font(.system(size: 11.5, design: .monospaced))
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            }
            if events.isEmpty { Text("No analytics events found in this call.").foregroundStyle(Theme.faint) }
        }
    }
}
