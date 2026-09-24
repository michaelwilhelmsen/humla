import CoreML
import DiarizeCore
import FluidAudio
import Foundation

// Every command writes one JSON payload to stdout (download streams progress lines
// before it) and exits 0, or exits 1 with a `humla-error:` line on stderr.
//
//   speaker-diarize <wav-path> [--engine community1|nemotron3] [--num-speakers N] [--threshold T]
//   speaker-diarize status|download|delete [--engine community1|nemotron3]
//
// `--num-speakers` and `--threshold` apply to community-1 only: Nemotron 3 counts
// speakers itself, up to eight.

let args = CommandLine.arguments

func writeStderr(_ msg: String) {
    FileHandle.standardError.write(Data("\(msg)\n".utf8))
}

func writeStdout(_ obj: Any) {
    if let data = try? JSONSerialization.data(withJSONObject: obj),
       let s = String(data: data, encoding: .utf8) {
        print(s)
        fflush(stdout)
    }
}

/// The segment payload goes straight to fd 1: a stdio-buffered `print()` inside
/// FluidAudio would otherwise flush around it.
func writePayload(_ payload: [[String: Any]]) throws {
    let data = try JSONSerialization.data(withJSONObject: payload)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

func usage() -> Never {
    writeStderr(
        "usage: speaker-diarize (<wav-path>|status|download|delete) [--engine community1|nemotron3] [--num-speakers N] [--threshold T]"
    )
    exit(2)
}

guard args.count >= 2 else { usage() }

enum Engine: String {
    case community1
    case nemotron3
}

let engine: Engine = {
    guard let i = args.firstIndex(of: "--engine") else { return .community1 }
    guard i + 1 < args.count, let parsed = Engine(rawValue: args[i + 1]) else { usage() }
    return parsed
}()

func parseDoubleFlag(_ flag: String) -> Double? {
    guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
    return Double(args[i + 1])
}

// CoreML models are .mlmodelc directories, so sizes have to be summed recursively.
func directorySize(_ url: URL) -> Int64 {
    let enumerator = FileManager.default.enumerator(
        at: url,
        includingPropertiesForKeys: [.fileSizeKey, .isRegularFileKey],
        options: [.skipsHiddenFiles]
    )
    var total: Int64 = 0
    while let item = enumerator?.nextObject() as? URL {
        if let v = try? item.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]),
           v.isRegularFile == true,
           let size = v.fileSize {
            total += Int64(size)
        }
    }
    return total
}

func fluidAudioModelsRoot() -> URL {
    FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("FluidAudio/Models", isDirectory: true)
}

func community1ModelsDirectory() -> URL {
    OfflineDiarizerModels
        .defaultModelsDirectory()
        .appendingPathComponent(Repo.diarizer.folderName, isDirectory: true)
}

// fp16 `fast128` on the Neural Engine: the split W8A8 presets output ~0.5 for every
// speaker on M1-class Neural Engines, and `offline` cannot compile for it at all.
let nemotronConfig = Nemotron3Config.fast128
let nemotronComputeUnits: MLComputeUnits = .cpuAndNeuralEngine

func nemotronModelsDirectory() -> URL {
    fluidAudioModelsRoot()
        .appendingPathComponent(Repo.nemotron3Diarization.folderName, isDirectory: true)
}

func readMarker(_ url: URL) -> String? {
    (try? String(contentsOf: url, encoding: .utf8))?
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

/// Whether the preset's bundle and assets are on disk from the current weights.
/// FluidAudio discards a cache from superseded weights on the next load, which
/// would then download inside the post-stop chain.
func nemotronFilesPresent() -> Bool {
    let dir = nemotronModelsDirectory()
    let fm = FileManager.default
    let bundle = dir
        .appendingPathComponent(nemotronConfig.hubSubdirectory)
        .appendingPathComponent(nemotronConfig.modelFileName)
    return readMarker(dir.appendingPathComponent(ModelNames.Nemotron3.weightsVersionFile))
        == ModelNames.Nemotron3.weightsVersion
        && fm.fileExists(atPath: bundle.appendingPathComponent("coremldata.bin").path)
        && fm.fileExists(
            atPath: dir.appendingPathComponent(ModelNames.Nemotron3.silenceEmbeddingFile).path)
}

/// Written once a download's warm-up inference has run. CoreML keeps a Neural
/// Engine compile per app, so the marker is named for this one: the bundled
/// sidecar reports the app's bundle id, a bare binary its own name. FluidAudio
/// writes its weights marker before any load, so that one can't say this.
func nemotronWarmMarker() -> URL {
    let identity = Bundle.main.bundleIdentifier ?? ProcessInfo.processInfo.processName
    return nemotronModelsDirectory().appendingPathComponent(".humla-warm-\(identity)")
}

/// Downloaded and compiled for the Neural Engine, so a diarize run neither
/// fetches nor compiles after a recording stops.
func nemotronIsReady() -> Bool {
    nemotronFilesPresent() && readMarker(nemotronWarmMarker()) == ModelNames.Nemotron3.weightsVersion
}

// MARK: - Status

func writeStatus(downloaded: Bool, dir: URL) {
    guard FileManager.default.fileExists(atPath: dir.path) else {
        writeStdout([
            "downloaded": false,
            "path": NSNull(),
            "sizeBytes": NSNull(),
        ] as [String: Any])
        return
    }
    writeStdout([
        "downloaded": downloaded,
        "path": dir.path,
        "sizeBytes": directorySize(dir),
    ] as [String: Any])
}

func runStatus() {
    switch engine {
    case .community1:
        let dir = community1ModelsDirectory()
        let allPresent = ModelNames.OfflineDiarizer.requiredModels.allSatisfy {
            FileManager.default.fileExists(atPath: dir.appendingPathComponent($0).path)
        }
        writeStatus(downloaded: allPresent, dir: dir)
    case .nemotron3:
        writeStatus(downloaded: nemotronIsReady(), dir: nemotronModelsDirectory())
    }
}

// MARK: - Delete

func runDelete() {
    let dir: URL
    switch engine {
    case .community1: dir = community1ModelsDirectory()
    case .nemotron3: dir = nemotronModelsDirectory()
    }
    if FileManager.default.fileExists(atPath: dir.path) {
        try? FileManager.default.removeItem(at: dir)
    }
    writeStdout(["deleted": true, "path": dir.path])
}

// MARK: - Download

@Sendable func writeProgress(fraction: Double, phase: String) {
    writeStdout([
        "event": "progress",
        "fraction": fraction,
        "phase": phase,
    ] as [String: Any])
}

let forwardDownloadProgress: ProgressHandler = { progress in
    let phase: String
    switch progress.phase {
    case .listing: phase = "listing"
    case .downloading: phase = "downloading"
    case .compiling: phase = "compiling"
    }
    writeProgress(fraction: progress.fractionCompleted, phase: phase)
}

func runDownloadCommunity1() async -> Int32 {
    do {
        _ = try await OfflineDiarizerModels.load(progressHandler: forwardDownloadProgress)
        writeStdout(["event": "done"])
        return 0
    } catch {
        writeStderr("download error: \(error)")
        return 1
    }
}

func runDownloadNemotron() async -> Int32 {
    do {
        // Fetched and loaded on the CPU first, which is quick, so that the Neural
        // Engine compile below reports as a phase of its own.
        _ = try await Nemotron3Models.loadFromHuggingFace(
            config: nemotronConfig,
            computeUnits: .cpuOnly,
            progressHandler: forwardDownloadProgress
        )
        // The first Neural Engine load compiles the model for this Mac, which takes
        // minutes, and CoreML caches the result per app and macOS build. Running one
        // inference here keeps that out of the post-stop chain.
        writeProgress(fraction: 0, phase: "warming")
        let models = try await Nemotron3Models.loadFromHuggingFace(
            config: nemotronConfig,
            computeUnits: nemotronComputeUnits
        )
        _ = try Nemotron3Diarizer(config: nemotronConfig, models: models)
            .processComplete([Float](repeating: 0, count: 16_000))
        try Data((ModelNames.Nemotron3.weightsVersion + "\n").utf8)
            .write(to: nemotronWarmMarker(), options: .atomic)
        writeStdout(["event": "done"])
        return 0
    } catch {
        writeStderr("download error: \(error)")
        return 1
    }
}

// MARK: - Diarize: community-1

func runDiarizeCommunity1(audioPath: String, numSpeakers: Int?, threshold: Double?) async -> Int32 {
    do {
        // Segments shorter than 1 s and pauses shorter than 0.5 s are merged into
        // their surroundings, so a backchannel inside a monologue doesn't split one
        // sentence across three transcript lines. Overlap stays excluded from the
        // embeddings, and output segments stay non-overlapping so each word maps to
        // exactly one speaker.
        var config = OfflineDiarizerConfig(
            clusteringThreshold: fluidAudioClusteringThreshold(fromStored: threshold ?? 0.5),
            segmentationMinDurationOn: 1.0,
            segmentationMinDurationOff: 0.5
        )
        // Without a count, VBx picks its own and tends to choose one on
        // conversations one person dominates.
        if let n = numSpeakers, n > 0 {
            config = config.withSpeakers(exactly: n)
        }
        let manager = OfflineDiarizerManager(config: config)
        try await manager.prepareModels()

        let result: DiarizationResult
        do {
            result = try await manager.process(URL(fileURLWithPath: audioPath))
        } catch OfflineDiarizationError.noSpeechDetected {
            // Too short or too quiet to hold speech: no segments, not a failure.
            try writePayload([])
            return 0
        }

        try writePayload(result.segments.map { seg in
            [
                "start_ms": Int(seg.startTimeSeconds * 1000.0),
                "end_ms": Int(seg.endTimeSeconds * 1000.0),
                "speaker_id": seg.speakerId,
            ]
        })
        return 0
    } catch {
        // Tagged so the Rust side can pick it out of FluidAudio's stderr logging.
        writeStderr("humla-error: \(error.localizedDescription)")
        return 1
    }
}

// MARK: - Diarize: Nemotron 3

func runDiarizeNemotron(audioPath: String) async -> Int32 {
    do {
        // Loading a missing model would download it here, after a recording stopped.
        guard nemotronFilesPresent() else {
            writeStderr("humla-error: Nemotron 3 model not downloaded")
            return 1
        }
        let models = try await Nemotron3Models.loadFromHuggingFace(
            config: nemotronConfig,
            computeUnits: nemotronComputeUnits
        )
        let diarizer = Nemotron3Diarizer(config: nemotronConfig, models: models)
        let audio = try AudioConverter().resampleAudioFile(path: audioPath)
        let (probabilities, frameCount) = try diarizer.processComplete(audio)
        let segments = Nemotron3Diarizer.segments(
            probabilities: probabilities,
            frameCount: frameCount,
            numSpeakers: nemotronConfig.numSpeakers
        )
        try writePayload(segments.map { seg in
            [
                "start_ms": Int((seg.startSeconds * 1000).rounded()),
                "end_ms": Int((seg.endSeconds * 1000).rounded()),
                "speaker_id": "S\(seg.speakerIndex)",
            ]
        })
        return 0
    } catch {
        writeStderr("humla-error: \(error.localizedDescription)")
        return 1
    }
}

let semaphore = DispatchSemaphore(value: 0)
var exitCode: Int32 = 0

switch args[1] {
case "status":
    runStatus()
case "delete":
    runDelete()
case "download":
    Task {
        switch engine {
        case .community1: exitCode = await runDownloadCommunity1()
        case .nemotron3: exitCode = await runDownloadNemotron()
        }
        semaphore.signal()
    }
    semaphore.wait()
default:
    let path = args[1]
    var numSpeakers: Int? = nil
    if let i = args.firstIndex(of: "--num-speakers"),
       i + 1 < args.count,
       let n = Int(args[i + 1]),
       n > 0 {
        numSpeakers = n
    }
    let clusteringThreshold = parseDoubleFlag("--threshold")
    Task {
        switch engine {
        case .community1:
            exitCode = await runDiarizeCommunity1(
                audioPath: path,
                numSpeakers: numSpeakers,
                threshold: clusteringThreshold
            )
        case .nemotron3:
            exitCode = await runDiarizeNemotron(audioPath: path)
        }
        semaphore.signal()
    }
    semaphore.wait()
}

exit(exitCode)
