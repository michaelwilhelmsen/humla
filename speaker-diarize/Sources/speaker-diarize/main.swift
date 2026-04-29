import Foundation
import FluidAudio

// Sidecar entrypoints. All commands write a single JSON payload to stdout
// (with download additionally streaming progress lines as JSON before the
// final payload). Exit 0 on success, 1 on failure with stderr message.
//
//   speaker-diarize <wav-path>   — run diarization on a WAV file
//   speaker-diarize status       — model presence + size on disk
//   speaker-diarize download     — download + compile the model (streams progress)
//   speaker-diarize delete       — wipe the cached model directory

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

func runStatus() {
    let dir = DiarizerModels.defaultModelsDirectory()
    let exists = FileManager.default.fileExists(atPath: dir.path)
    if !exists {
        writeStdout([
            "downloaded": false,
            "path": NSNull(),
            "sizeBytes": NSNull(),
        ] as [String: Any])
        return
    }
    // Treat the directory as "downloaded" iff every required model file is
    // present underneath it. Partial-download leftovers report as
    // not-downloaded so the UI prompts a re-download instead of pretending
    // diarization will work.
    let required = DiarizerModels.requiredModelNames
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
    let dir = DiarizerModels.defaultModelsDirectory()
    if FileManager.default.fileExists(atPath: dir.path) {
        try? FileManager.default.removeItem(at: dir)
    }
    writeStdout(["deleted": true, "path": dir.path])
}

func runDownload() async -> Int32 {
    do {
        // FluidAudio's progressHandler is invoked during the underlying
        // HuggingFace fetch + Core ML compile. Forward each call to stdout
        // so the Rust side can emit Tauri events.
        let _ = try await DiarizerModels.downloadIfNeeded(progressHandler: { progress in
            // FluidAudio progress goes through three phases: listing files,
            // downloading them, and compiling the CoreML models for the
            // Apple Neural Engine. We surface a phase tag so the UI can
            // distinguish the download vs the (slower) compile step.
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

func runDiarize(audioPath: String) async -> Int32 {
    do {
        let models = try await DiarizerModels.downloadIfNeeded()
        // 0.5 leans aggressive on speaker SEPARATION (lower threshold ⇒ more
        // speakers detected). YouTube / Teams / system-audio captures put
        // multiple voices through the same downstream codec, which makes
        // their embeddings sit closer together than they would in clean
        // multi-channel recordings — at threshold 0.6 (and especially 0.7)
        // the model tends to merge them.
        let config = DiarizerConfig(clusteringThreshold: 0.5)
        let diarizer = DiarizerManager(config: config)
        diarizer.initialize(models: models)

        let converter = AudioConverter()
        let url = URL(fileURLWithPath: audioPath)
        let samples = try converter.resampleAudioFile(url)

        let result = try await diarizer.performCompleteDiarization(samples)

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
    let path = args[1]
    Task {
        exitCode = await runDiarize(audioPath: path)
        semaphore.signal()
    }
    semaphore.wait()
}

exit(exitCode)
