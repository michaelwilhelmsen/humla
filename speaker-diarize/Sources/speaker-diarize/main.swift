import Foundation
import FluidAudio

// Sidecar entrypoint. Usage:
//   speaker-diarize <wav-path>
//
// Writes a single JSON array to stdout: [{start_ms, end_ms, speaker_id}, ...]
// Exits 0 on success, 1 on any failure (with a single-line error written
// to stderr — the Rust caller logs it and falls back to an untagged
// transcript).

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: speaker-diarize <wav-path>\n".utf8))
    exit(2)
}
let audioPath = args[1]

func failHard(_ message: String) -> Never {
    FileHandle.standardError.write(Data("\(message)\n".utf8))
    exit(1)
}

// FluidAudio uses async APIs (model download is awaitable). Drive a single
// task to completion via a semaphore so the binary acts as a synchronous
// CLI from the caller's perspective.
let semaphore = DispatchSemaphore(value: 0)
var exitCode: Int32 = 1

Task {
    defer { semaphore.signal() }
    do {
        // First-run downloads ~500 MB of CoreML models from the public
        // FluidInference HuggingFace mirror; subsequent runs hit the cache.
        let models = try await DiarizerModels.downloadIfNeeded()
        let diarizer = DiarizerManager()
        diarizer.initialize(models: models)

        // Resample / convert to the 16 kHz mono Float32 the model expects.
        // The sidecar already writes WAVs in this format, but going through
        // the converter is cheap insurance against future format drift.
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
        exitCode = 0
    } catch {
        FileHandle.standardError.write(Data("speaker-diarize error: \(error)\n".utf8))
    }
}

semaphore.wait()
exit(exitCode)
