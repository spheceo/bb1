// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "bb1",
    platforms: [.macOS(.v14)],
    targets: [
        .target(name: "BB1Core"),
        // One app for both roles: `bb1` (GUI, mode from settings), `bb1 host …`, `bb1 client …`.
        .executableTarget(name: "bb1", dependencies: ["BB1Core"]),
        .executableTarget(name: "bb1-selftest", dependencies: ["BB1Core"]),
    ]
)
