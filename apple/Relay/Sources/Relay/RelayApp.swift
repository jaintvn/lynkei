import AppKit
import SwiftUI

@main
struct RelayApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var store: TrafficStore
    @State private var island: IslandController

    init() {
        let s = TrafficStore()
        _store = State(initialValue: s)
        _island = State(initialValue: IslandController(store: s))
    }

    var body: some Scene {
        Window("Relay", id: "main") {
            ContentView()
                .environment(store)
                .frame(minWidth: 1400, minHeight: 700)
                .onAppear {
                    store.start()
                    MacCertificate.shared.onInstalled = { store.retryDecrypting() }
                    island.start()
                }
                .onOpenURL { url in SessionIO.open(url, store: store) }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in store.shutdown() }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1440, height: 900)
        .commands {
            CommandGroup(after: .help) {
                Button("Install Relay Certificate on This Mac…") { MacCertificate.shared.install() }
                Button("Show Relay Certificate in Finder") { MacCertificate.shared.revealFile() }
            }
            CommandGroup(after: .newItem) {
                Button("Open Session…") { SessionIO.open(store: store) }.keyboardShortcut("o")
                Button("Share Session…") { SessionIO.share(store: store) }.keyboardShortcut("s", modifiers: [.command, .shift])
            }
        }
        MenuBarExtra("Relay", systemImage: "arrow.triangle.swap") {
            RelayMenuBar(island: island, store: store)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // Running from `swift run` gives no bundle; make it a regular foreground app with a Dock icon.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
