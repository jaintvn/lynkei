// swift-tools-version: 6.0
// Relay for Mac — P0 spike app. Run with: swift run --package-path apple/Relay
import PackageDescription

let package = Package(
    name: "Relay",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(path: "../RelayKit"),
    ],
    targets: [
        .executableTarget(
            name: "Relay",
            dependencies: [.product(name: "RelayKit", package: "RelayKit")],
            path: "Sources/Relay"
        ),
    ]
)
