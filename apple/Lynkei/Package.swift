// swift-tools-version: 6.0
// Lynkei for Mac — P0 spike app. Run with: swift run --package-path apple/Lynkei
import PackageDescription

let package = Package(
    name: "Lynkei",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(path: "../LynkeiKit"),
    ],
    targets: [
        .executableTarget(
            name: "Lynkei",
            dependencies: [.product(name: "LynkeiKit", package: "LynkeiKit")],
            path: "Sources/Lynkei"
        ),
    ]
)
