import Foundation
import FluidAudio

// Sidecar entrypoints. All commands write a single JSON payload to stdout
// (with download additionally streaming progress lines as JSON before the
// final payload). Exit 0 on success, 1 on failure with stderr message.
//
//   speaker-diarize <wav-path> [--num-speakers N] [--engine community1|sortformer]
//                                — run offline diarization on a WAV file.
//                                  Optional `--num-speakers N` pins the
//                                  cluster count when the caller knows it
//                                  (e.g. "I'm in a 1:1 with one other
//                                  person → N=2"). Without the flag, VBx
//                                  decides cluster count on its own —
//                                  which under-counts on conversations
//                                  dominated by one speaker. (community1
//                                  only — Sortformer has a fixed 4-speaker
//                                  cap and ignores the hint.)
//   speaker-diarize status   [--engine community1|sortformer]
//                                — model presence + size on disk
//   speaker-diarize download [--engine community1|sortformer]
//                                — download + compile (streams progress)
//   speaker-diarize delete   [--engine community1|sortformer]
//                                — wipe the cached model directory
//
// Default engine is `community1` (FluidAudio's `OfflineDiarizerManager` —
// community-1 segmentation + VBx clustering with PLDA score normalisation).
// The `sortformer` engine swaps in NVIDIA's Streaming Sortformer (4-speaker
// end-to-end transformer) running in batch mode via `SortformerDiarizer.
// processComplete(audioFileURL:)`. Sortformer trades the clustering
// approach's cleanliness for materially better behaviour on rapid
// speaker changes within a channel — the failure mode community-1 hits
// its architectural ceiling on. We use the `highContextV2_1` variant
// which expands chunkRightContext from 7 to 40 frames (~4s of right-side
// lookahead) for the offline accuracy we want here, not the streaming
// latency the default `fastV2_1` is tuned for.

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

guard args.count >= 2 else {
    writeStderr(
        "usage: speaker-diarize (<wav-path>|status|download|delete) [--engine community1|sortformer]"
    )
    exit(2)
}

enum Engine: String {
    case community1
    case sortformer
}

func parseEngine(_ args: [String]) -> Engine {
    if let i = args.firstIndex(of: "--engine"), i + 1 < args.count {
        return Engine(rawValue: args[i + 1]) ?? .community1
    }
    return .community1
}

func parseFloatFlag(_ args: [String], _ flag: String) -> Float? {
    guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
    return Float(args[i + 1])
}

func parseDoubleFlag(_ args: [String], _ flag: String) -> Double? {
    guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
    return Double(args[i + 1])
}

let engine = parseEngine(args)

// CoreML models are .mlmodelc directories — `.size` on a directory only
// reports the directory entry, not its contents. Walk recursively.
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

// FluidAudio stores the offline diarizer files under
// <FluidAudio/Models>/speaker-diarization/ — the parent dir comes from
// `OfflineDiarizerModels.defaultModelsDirectory()`, the subdir name from
// `Repo.diarizer.folderName`. We resolve the leaf path here so status/delete
// inspect exactly what `OfflineDiarizerModels.load` writes.
func community1ModelsDirectory() -> URL {
    OfflineDiarizerModels
        .defaultModelsDirectory()
        .appendingPathComponent(Repo.diarizer.folderName, isDirectory: true)
}

// Sortformer models live under
// <Library/Application Support/FluidAudio/Models>/diar-streaming-sortformer-coreml/
// per `SortformerModels.loadFromHuggingFace`'s default cache layout. We use
// the highContextV2_1 variant for offline accuracy.
let sortformerVariant: ModelNames.Sortformer.Variant = .highContextV2_1
let sortformerConfig: SortformerConfig = .highContextV2_1

func sortformerModelsDirectory() -> URL {
    FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("FluidAudio/Models", isDirectory: true)
        .appendingPathComponent("diar-streaming-sortformer-coreml", isDirectory: true)
}

func sortformerModelPath() -> URL {
    sortformerModelsDirectory()
        .appendingPathComponent(sortformerVariant.fileName, isDirectory: true)
}

// MARK: - Status

func runStatusCommunity1() {
    let dir = community1ModelsDirectory()
    let exists = FileManager.default.fileExists(atPath: dir.path)
    if !exists {
        writeStdout([
            "downloaded": false,
            "path": NSNull(),
            "sizeBytes": NSNull(),
        ] as [String: Any])
        return
    }
    let required = ModelNames.OfflineDiarizer.requiredModels
    var allPresent = true
    for name in required {
        let modelURL = dir.appendingPathComponent(name)
        if !FileManager.default.fileExists(atPath: modelURL.path) {
            allPresent = false
            break
        }
    }
    let size = directorySize(dir)
    writeStdout([
        "downloaded": allPresent,
        "path": dir.path,
        "sizeBytes": size,
    ] as [String: Any])
}

func runStatusSortformer() {
    let modelPath = sortformerModelPath()
    let exists = FileManager.default.fileExists(atPath: modelPath.path)
    if !exists {
        writeStdout([
            "downloaded": false,
            "path": NSNull(),
            "sizeBytes": NSNull(),
        ] as [String: Any])
        return
    }
    let size = directorySize(modelPath)
    writeStdout([
        "downloaded": true,
        "path": modelPath.path,
        "sizeBytes": size,
    ] as [String: Any])
}

// MARK: - Delete

func runDelete() {
    let dir: URL
    switch engine {
    case .community1: dir = community1ModelsDirectory()
    case .sortformer: dir = sortformerModelsDirectory()
    }
    if FileManager.default.fileExists(atPath: dir.path) {
        try? FileManager.default.removeItem(at: dir)
    }
    writeStdout(["deleted": true, "path": dir.path])
}

// MARK: - Download

func runDownloadCommunity1() async -> Int32 {
    do {
        // `OfflineDiarizerModels.load` triggers downloadIfNeeded under the
        // hood and surfaces the same DownloadProgress phases as the streaming
        // path used to. Forward each tick to stdout so the Rust side can emit
        // Tauri events to the UI progress bar.
        _ = try await OfflineDiarizerModels.load(progressHandler: { progress in
            let phase: String
            switch progress.phase {
            case .listing: phase = "listing"
            case .downloading: phase = "downloading"
            case .compiling: phase = "compiling"
            }
            writeStdout([
                "event": "progress",
                "fraction": progress.fractionCompleted,
                "phase": phase,
            ] as [String: Any])
        })
        writeStdout(["event": "done"])
        return 0
    } catch {
        writeStderr("download error: \(error)")
        return 1
    }
}

func runDownloadSortformer() async -> Int32 {
    do {
        _ = try await SortformerModels.loadFromHuggingFace(
            config: sortformerConfig,
            progressHandler: { progress in
                let phase: String
                switch progress.phase {
                case .listing: phase = "listing"
                case .downloading: phase = "downloading"
                case .compiling: phase = "compiling"
                }
                writeStdout([
                    "event": "progress",
                    "fraction": progress.fractionCompleted,
                    "phase": phase,
                ] as [String: Any])
            }
        )
        writeStdout(["event": "done"])
        return 0
    } catch {
        writeStderr("download error: \(error)")
        return 1
    }
}

// MARK: - Diarize: community1

func runDiarizeCommunity1(audioPath: String, numSpeakers: Int?, threshold: Double?) async -> Int32 {
    do {
        // Tuning notes for in-person meetings on a shared mic:
        //   - clusteringThreshold default 0.4 (down from community default 0.6,
        //     and down from the 0.5 we shipped initially) so similar-sounding
        //     voices recorded in the same room don't collapse onto one
        //     cluster. Lower = more aggressive separation. The value was
        //     tightened after observing the v0.8.0 build still merging
        //     two-person conversations into a single cluster when one
        //     speaker dominated and the other only dropped short
        //     interjections. Caller can override via --threshold.
        //   - excludeOverlap stays true (default): when two speakers overlap,
        //     the overlapping frames are masked out before extracting per-
        //     speaker embeddings, so the embedding stays clean.
        //   - exclusiveSegments stays true (default): output is non-overlapping
        //     so each chunk maps to exactly one speaker for the chunk-to-
        //     segment alignment in commands.rs::assign_speaker.
        var config = OfflineDiarizerConfig(clusteringThreshold: threshold ?? 0.4)
        // Caller-supplied speaker count hint when the user knows the count
        // ahead of time. `withSpeakers(exactly:)` overrides the auto cluster
        // detection inside VBx — without it, VBx is free to pick any
        // count, and on dominant-speaker conversations it tends to choose 1.
        if let n = numSpeakers, n > 0 {
            config = config.withSpeakers(exactly: n)
        }
        let manager = OfflineDiarizerManager(config: config)
        try await manager.prepareModels()

        let url = URL(fileURLWithPath: audioPath)
        let result: DiarizationResult
        do {
            result = try await manager.process(url)
        } catch OfflineDiarizationError.noSpeechDetected {
            // FluidAudio raises noSpeechDetected when its segmentation
            // model finds no speech frames in the audio (very short or
            // very quiet recordings, sometimes the brief moment between
            // VAD chunks closing and stop being signalled). This isn't
            // an error — surface as an empty segment array so the Rust
            // side's existing "no segments" graceful path handles it
            // (skip in mic-only / sys-only, single-speaker fallback in
            // hybrid). Avoids dumping a wall of FluidAudio profiling
            // logs into the user's recording-error toast.
            let data = try JSONSerialization.data(withJSONObject: [] as [Any])
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data("\n".utf8))
            return 0
        }

        // Output shape stays identical to the previous DiarizerManager path:
        // an array of {start_ms, end_ms, speaker_id} that the Rust side can
        // align to chunks via start_ms.
        let payload: [[String: Any]] = result.segments.map { seg in
            [
                "start_ms": Int(seg.startTimeSeconds * 1000.0),
                "end_ms": Int(seg.endTimeSeconds * 1000.0),
                "speaker_id": seg.speakerId,
            ]
        }
        let data = try JSONSerialization.data(withJSONObject: payload)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
        return 0
    } catch {
        // Tag the actual error on its own dedicated line so the Rust
        // side can pluck it out of FluidAudio's profiling stderr noise
        // for the user toast. Profiling output goes to stderr in front
        // of this line; we only want THIS line to reach the UI.
        writeStderr("humla-error: \(error.localizedDescription)")
        return 1
    }
}

// MARK: - Diarize: Sortformer

func runDiarizeSortformer(
    audioPath: String,
    silenceThreshold: Float?,
    predScoreThreshold: Float?
) async -> Int32 {
    do {
        let modelPath = sortformerModelPath()
        guard FileManager.default.fileExists(atPath: modelPath.path) else {
            writeStderr("humla-error: Sortformer model not downloaded")
            return 1
        }

        // SortformerConfig is a struct of `var` thresholds, so we can clone
        // the highContextV2_1 default and override the two we expose.
        var config = sortformerConfig
        if let s = silenceThreshold { config.silenceThreshold = s }
        if let p = predScoreThreshold { config.predScoreThreshold = p }

        let diarizer = SortformerDiarizer(config: config)
        try await diarizer.initialize(mainModelPath: modelPath)

        let url = URL(fileURLWithPath: audioPath)
        let timeline = try diarizer.processComplete(audioFileURL: url)

        // Flatten DiarizerTimeline → [{start_ms, end_ms, speaker_id}] in
        // the same shape the community-1 path emits. Each speaker holds its
        // own segments collection; merge them into a single time-sorted
        // array using a stable "S<slot>" speaker_id string. We pull from
        // both finalized and tentative buckets — processComplete with
        // finalizeOnCompletion=true (its default) confirms everything, but
        // tentative is kept in the union for robustness if that contract
        // ever changes upstream.
        var payload: [[String: Any]] = []
        for (slot, speaker) in timeline.speakers {
            let id = "S\(slot)"
            let segs = speaker.finalizedSegments + speaker.tentativeSegments
            for seg in segs {
                payload.append([
                    "start_ms": Int(seg.startTime * 1000.0),
                    "end_ms": Int(seg.endTime * 1000.0),
                    "speaker_id": id,
                ])
            }
        }
        payload.sort { (a, b) in
            (a["start_ms"] as? Int ?? 0) < (b["start_ms"] as? Int ?? 0)
        }
        let data = try JSONSerialization.data(withJSONObject: payload)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
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
    switch engine {
    case .community1: runStatusCommunity1()
    case .sortformer: runStatusSortformer()
    }
case "delete":
    runDelete()
case "download":
    Task {
        switch engine {
        case .community1: exitCode = await runDownloadCommunity1()
        case .sortformer: exitCode = await runDownloadSortformer()
        }
        semaphore.signal()
    }
    semaphore.wait()
default:
    // Positional <wav-path>, optional `--num-speakers N` and `--engine` flags.
    let path = args[1]
    var numSpeakers: Int? = nil
    if let i = args.firstIndex(of: "--num-speakers"),
       i + 1 < args.count,
       let n = Int(args[i + 1]),
       n > 0 {
        numSpeakers = n
    }
    let clusteringThreshold = parseDoubleFlag(args, "--threshold")
    let silenceThreshold = parseFloatFlag(args, "--silence-threshold")
    let predScoreThreshold = parseFloatFlag(args, "--pred-threshold")
    Task {
        switch engine {
        case .community1:
            exitCode = await runDiarizeCommunity1(
                audioPath: path,
                numSpeakers: numSpeakers,
                threshold: clusteringThreshold
            )
        case .sortformer:
            exitCode = await runDiarizeSortformer(
                audioPath: path,
                silenceThreshold: silenceThreshold,
                predScoreThreshold: predScoreThreshold
            )
        }
        semaphore.signal()
    }
    semaphore.wait()
}

exit(exitCode)
