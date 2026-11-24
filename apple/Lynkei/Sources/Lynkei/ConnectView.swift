import AppKit
import CoreImage.CIFilterBuiltins
import LynkeiKit
import SwiftUI

/// "Connect a device": auto-detected cards with a three-step checklist, plus the iPhone QR card.
struct ConnectView: View {
    @Environment(TrafficStore.self) private var traffic
    @Environment(\.dismiss) private var dismiss
    @State private var devices: DeviceStore?
    @State private var usbPhones: [PhysicalPhone] = []

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(Theme.line)
            ScrollView {
                if let devices {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 16, alignment: .top)], spacing: 16) {
                        MacCertificateCard()
                        ForEach(devices.devices, id: \.id) { d in
                            DeviceCard(device: d, store: devices, trafficSeen: trafficSeen(after: devices.results[d.id]?.connectedAtMs))
                        }
                        ForEach(NetworkClient.detect(from: traffic.transactions)) { client in
                            NetworkClientCard(client: client)
                        }
                        if !usbPhones.isEmpty { UsbIPhoneCard(phones: usbPhones, setupURL: devices.iphoneSetupURL) }
                        WifiCard(url: devices.iphoneSetupURL, altURL: devices.wifiURL, trafficSeen: false)
                        AndroidWifiCard(url: devices.androidWifiURL)
                    }
                    .padding(24)
                    if devices.devices.isEmpty {
                        Text("No simulators or Android devices found. Boot one in Xcode or Android Studio, or plug in a phone with USB debugging, and it appears here.")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.muted)
                            .padding(.horizontal, 24)
                            .padding(.bottom, 24)
                    }
                }
            }
            Divider().overlay(Theme.line)
            footer
        }
        .frame(width: 1000, height: 640)
        .background(Theme.panel)
        .foregroundStyle(Theme.fg)
        .onAppear {
            let s = traffic.deviceStore()
            devices = s
            s.startWatching()
        }
        .task {
            // Look for cabled iPhones while the sheet is open.
            while !Task.isCancelled {
                usbPhones = await Task.detached { UsbIPhones.detect() }.value
                try? await Task.sleep(for: .seconds(8))
            }
        }
        .onDisappear { devices?.stopWatching() }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Step 1 of 1 — that’s it").font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.accent)
                Text("Connect a device").font(.system(size: 26, weight: .semibold))
                Text("Lynkei found these on your Mac. Pick one and it sets up the proxy and certificate for you. Nothing to configure by hand.")
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.muted)
                    .frame(maxWidth: 560, alignment: .leading)
            }
            Spacer()
            HStack(spacing: 8) {
                Circle().fill(traffic.isRunning ? Theme.s2xx : Theme.faint).frame(width: 8, height: 8)
                Text(traffic.isRunning ? "Proxy listening on" : "Proxy stopped").foregroundStyle(Theme.muted)
                Text("\(Host.current().localizedName ?? "this Mac"):\(traffic.port.map(String.init) ?? "—")")
                    .font(Theme.mono)
            }
            .font(.system(size: 12.5))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 22)
    }

    private var footer: some View {
        HStack {
            Text("Disconnecting undoes every change Lynkei made.").font(.system(size: 12.5)).foregroundStyle(Theme.muted)
            Spacer()
            Button("Done") { dismiss() }.buttonStyle(PillButtonStyle())
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 14)
        .background(Theme.raised)
    }

    /// Any request recorded after the device was connected counts as "traffic seen".
    private func trafficSeen(after ms: UInt64?) -> Bool {
        guard let ms else { return false }
        return traffic.transactions.contains { $0.startedAtMs >= ms }
    }
}

// MARK: - Cards

struct DeviceCard: View {
    let device: FfiDevice
    let store: DeviceStore
    let trafficSeen: Bool

    var body: some View {
        let result = store.results[device.id]
        let connected = store.isConnected(device)
        let busy = store.busy.contains(device.id)
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .font(.system(size: 18))
                    .foregroundStyle(device.kind.hasPrefix("android") ? Theme.s2xx : Theme.fg)
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text(kindLabel).font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text(device.name).font(.system(size: 15, weight: .semibold))
                    Text(device.os).font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                ChecklistRow(title: "Proxy", step: result?.proxy, busy: busy)
                ChecklistRow(title: "Certificate trusted", step: result?.certificate, busy: busy)
                ChecklistRow(
                    title: "Traffic seen",
                    step: connected ? FfiStep(state: trafficSeen ? "done" : "pending", detail: trafficSeen ? "Requests are flowing" : "Waiting for the first request. Open an app on the device and use it.") : nil,
                    busy: false
                )
            }
            if let message = store.errors[device.id] {
                Text(message).font(.system(size: 12)).foregroundStyle(Theme.s5xx)
            }
            if device.kind == "ios_simulator", connected, !MacProxyStatus.shared.isLynkei, MacProxyStatus.shared.state != .unknown {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.s4xx)
                    VStack(alignment: .leading, spacing: 4) {
                        Text("The Mac's proxy is off, so this simulator is not going through Lynkei.").font(.system(size: 12, weight: .medium))
                        Button("Turn the proxy on again") { Task { await store.connect(device) } }
                            .buttonStyle(PillButtonStyle()).font(.system(size: 12, weight: .medium))
                    }
                }
                .padding(10)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.s4xx.opacity(0.10)))
            }
            Button(action: {
                Task { connected ? await store.disconnect(device) : await store.connect(device) }
            }) {
                Text(busy ? "Working…" : (connected ? "Disconnect" : "Connect"))
                    .font(.system(size: 13, weight: .semibold))
                    .frame(maxWidth: .infinity)
                    .frame(height: 34)
                    .foregroundStyle(connected ? Theme.fg : Theme.bg)
                    .background(RoundedRectangle(cornerRadius: 9).fill(connected ? Color.clear : Theme.accent))
                    .overlay(RoundedRectangle(cornerRadius: 9).stroke(connected ? Theme.line2 : Color.clear))
            }
            .buttonStyle(.plain)
            .disabled(busy)
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(connected ? Theme.accent.opacity(0.55) : Theme.line))
    }

    private var icon: String {
        switch device.kind {
        case "ios_simulator": return "iphone"
        case "android_usb": return "cable.connector"
        default: return "iphone.gen3"
        }
    }

    private var kindLabel: String {
        switch device.kind {
        case "ios_simulator": return "IOS SIMULATOR"
        case "android_emulator": return "ANDROID EMULATOR"
        default: return "ANDROID OVER USB"
        }
    }
}

/// A device Lynkei found by its traffic (e.g. an iPhone on Wi-Fi), with the same checklist as the others.
struct NetworkClientCard: View {
    let client: NetworkClient

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: client.kind == .android ? "iphone.gen3" : "iphone")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text("FOUND ON YOUR WI-FI").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text(client.title).font(.system(size: 15, weight: .semibold))
                    Text(client.ip).font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                ChecklistRow(title: "Proxy", step: client.proxyStep, busy: false)
                ChecklistRow(title: "Certificate trusted", step: client.certificateStep, busy: false)
                ChecklistRow(title: "Traffic seen", step: client.trafficStep, busy: false)
            }
            Text("\(client.requests) requests · \(client.decrypted) decrypted · \(client.tunneled) passed through")
                .font(.system(size: 11.5))
                .foregroundStyle(Theme.muted)
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.accent.opacity(0.55)))
    }
}

struct WifiCard: View {
    let url: String
    /// The `<mac>.local` form, shown as a second address to try.
    var altURL: String = ""
    let trafficSeen: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "wifi")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text("IPHONE OVER WI-FI").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text("Your iPhone or iPad").font(.system(size: 15, weight: .semibold))
                    Text("Same network as this Mac").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            HStack(alignment: .center, spacing: 14) {
                if let qr = qrImage(for: url) {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 112, height: 112)
                        .padding(6)
                        .background(RoundedRectangle(cornerRadius: 8).fill(.white))
                        .accessibilityLabel("QR code that opens the Lynkei setup page")
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Scan with your iPhone camera").font(.system(size: 12.5, weight: .semibold))
                    Text("Installs the certificate, then shows the two settings to flip. Keeps working if this Mac’s IP changes.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.muted)
                    Text(url).font(.system(size: 10.5, design: .monospaced)).foregroundStyle(Theme.faint).textSelection(.enabled)
                }
            }
            .padding(12)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            VStack(alignment: .leading, spacing: 4) {
                Text("Page won't open?").font(.system(size: 11.5, weight: .semibold))
                Text("The phone and this Mac must be on the same Wi-Fi. Office and guest networks often block devices from reaching each other; then use a personal hotspot or an Android phone over USB. If the Mac's firewall is on, allow Lynkei in System Settings → Network → Firewall.\(altURL.isEmpty ? "" : " Or try \(altURL)")")
                    .font(.system(size: 11.5)).foregroundStyle(Theme.muted).textSelection(.enabled)
            }
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
    }
}

struct ChecklistRow: View {
    let title: String
    let step: FfiStep?
    let busy: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                Circle().stroke(color, lineWidth: 1.5).frame(width: 18, height: 18)
                if busy {
                    ProgressView().controlSize(.mini)
                } else if let step {
                    Image(systemName: symbol(step.state)).font(.system(size: 9, weight: .bold)).foregroundStyle(color)
                }
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: 12.5)).foregroundStyle(step == nil ? Theme.muted : Theme.fg)
                if let step, !step.detail.isEmpty {
                    Text(step.detail).font(.system(size: 11)).foregroundStyle(step.state == "failed" ? Theme.s5xx : Theme.muted)
                }
            }
        }
    }

    private var color: Color {
        switch step?.state {
        case "done": return Theme.s2xx
        case "needs_action": return Theme.s4xx
        case "failed": return Theme.s5xx
        default: return Theme.line2
        }
    }

    private func symbol(_ state: String) -> String {
        switch state {
        case "done": return "checkmark"
        case "needs_action": return "exclamationmark"
        case "failed": return "xmark"
        default: return "circle"
        }
    }
}

func qrImage(for text: String) -> NSImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data(text.utf8)
    filter.correctionLevel = "M"
    guard let out = filter.outputImage else { return nil }
    let scaled = out.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
    let rep = NSCIImageRep(ciImage: scaled)
    let image = NSImage(size: rep.size)
    image.addRepresentation(rep)
    return image
}
