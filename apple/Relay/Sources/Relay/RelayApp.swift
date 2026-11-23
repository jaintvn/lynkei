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
        WindowGroup("Relay") {
            ContentView()
                .environment(store)
                .frame(minWidth: 1400, minHeight: 700)
                .onAppear {
                    store.start()
                    island.start()
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in store.shutdown() }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1440, height: 900)
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
