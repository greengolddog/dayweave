// swift-tools-version: 6.1

import PackageDescription

let package = Package(
    name: "DayWeaveMac",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "DayWeave", targets: ["DayWeaveMac"]),
    ],
    targets: [
        .executableTarget(
            name: "DayWeaveMac",
            path: "Sources/DayWeaveMac"
        ),
        .testTarget(
            name: "DayWeaveMacTests",
            dependencies: ["DayWeaveMac"],
            path: "Tests/DayWeaveMacTests"
        ),
    ]
)

