import Foundation
import FluidAudio

// Sidecar entrypoints. All commands write a single JSON payload to stdout
// (with download additionally streaming progress lines as JSON before the
// final payload). Exit 0 on success, 1 on failure with stderr message.
//
//   speaker-diarize <wav-path> [--num-speakers N]
//                                — run offline diarization on a WAV file.
//                                  Optional `--num-speakers N` pins the
//                                  cluster count when the caller knows it
//                                  (e.g. "I'm in a 1:1 with one other
//                                  person → N=2"). Without the flag, VBx
//                                  decides cluster count on its own —
//                                  which under-counts on conversations
//                                  dominated by one speaker.
//   speaker-diarize status       — model presence + size on disk
//   speaker-diarize download     — download + compile the model (streams progress)
//   speaker-diarize delete       — wipe the cached model directory
//
// Backed by FluidAudio's `OfflineDiarizerManager` (community-1 segmentation +
// VBx clustering with PLDA score normalisation). This is the upgrade from the
// 3.1-based `DiarizerManager`, picked because community-1 counts and assigns
// speakers more accurately on dense single-mic captures (e.g. an in-person
// meeting where everyone shares one acoustic context — the failure mode that
// drove this change was different humans collapsing onto the same cluster).

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
    writeStderr("usage: speaker-diarize (<wav-path>|status|download|delete)")
    exit(2)
}

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
func offlineModelsDirectory() -> URL {
    OfflineDiarizerModels
        .defaultModelsDirectory()
        .appendingPathComponent(Repo.diarizer.folderName, isDirectory: true)
}

func runStatus() {
    let dir = offlineModelsDirectory()
    let exists = FileManager.default.fileExists(atPath: dir.path)
    if !exists {
        writeStdout([
            "downloaded": false,
            "path": NSNull(),
            "sizeBytes": NSNull(),
        ] as [String: Any])
        return
    }
    // Treat the directory as "downloaded" iff every required offline model
    // file (segmentation, fbank, embedding, plda) plus the plda-parameters
    // JSON is present. Partial-download leftovers report as not-downloaded
    // so the UI prompts a re-download instead of pretending diarization will
    // work.
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

func runDelete() {
    let dir = offlineModelsDirectory()
    if FileManager.default.fileExists(atPath: dir.path) {
        try? FileManager.default.removeItem(at: dir)
    }
    writeStdout(["deleted": true, "path": dir.path])
}

func runDownload() async -> Int32 {
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

func runDiarize(audioPath: String, numSpeakers: Int?) async -> Int32 {
    do {
        // Tuning notes for in-person meetings on a shared mic:
        //   - clusteringThreshold 0.4 (down from community default 0.6, and
        //     down from the 0.5 we shipped initially) so similar-sounding
        //     voices recorded in the same room don't collapse onto one
        //     cluster. Lower = more aggressive separation. The value was
        //     tightened after observing the v0.8.0 build still merging
        //     two-person conversations into a single cluster when one
        //     speaker dominated and the other only dropped short
        //     interjections.
        //   - excludeOverlap stays true (default): when two speakers overlap,
        //     the overlapping frames are masked out before extracting per-
        //     speaker embeddings, so the embedding stays clean.
        //   - exclusiveSegments stays true (default): output is non-overlapping
        //     so each chunk maps to exactly one speaker for the chunk-to-
        //     segment alignment in commands.rs::assign_speaker.
        var config = OfflineDiarizerConfig(clusteringThreshold: 0.4)
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
        let result = try await manager.process(url)

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
        writeStderr("speaker-diarize error: \(error)")
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
        exitCode = await runDownload()
        semaphore.signal()
    }
    semaphore.wait()
default:
    // Positional <wav-path>, optional `--num-speakers N` (in any position).
    let path = args[1]
    var numSpeakers: Int? = nil
    if let i = args.firstIndex(of: "--num-speakers"),
       i + 1 < args.count,
       let n = Int(args[i + 1]),
       n > 0 {
        numSpeakers = n
    }
    Task {
        exitCode = await runDiarize(audioPath: path, numSpeakers: numSpeakers)
        semaphore.signal()
    }
    semaphore.wait()
}

exit(exitCode)
