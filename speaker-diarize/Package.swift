// swift-tools-version:6.2
import PackageDescription

// Sidecar binary that runs FluidAudio's CoreML speaker diarization and prints
// segments as JSON to stdout. Spawned by the Rust backend after a recording stops.

let package = Package(
    name: "speaker-diarize",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Pinned exactly: FluidAudio has changed diarizer output in patch releases.
        // No traits, since the sidecar has no use for its text-normalization engine.
        .package(
            url: "https://github.com/FluidInference/FluidAudio.git",
            exact: "0.17.1",
            traits: []
        ),
    ],
    targets: [
        .target(
            name: "DiarizeCore",
            path: "Sources/DiarizeCore"
        ),
        .executableTarget(
            name: "speaker-diarize",
            dependencies: [
                "DiarizeCore",
                .product(name: "FluidAudio", package: "FluidAudio"),
            ],
            path: "Sources/speaker-diarize"
        ),
        .testTarget(
            name: "DiarizeCoreTests",
            dependencies: ["DiarizeCore"],
            path: "Tests/DiarizeCoreTests"
        ),
    ],
    swiftLanguageModes: [.v5]
)
