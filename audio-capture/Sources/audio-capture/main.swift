import Foundation
import AppKit
import AVFoundation
import ScreenCaptureKit
import CoreMedia
import CoreGraphics

// Hide the sidecar from the Dock and menu bar. ScreenCaptureKit and
// AVAudioEngine pull in AppKit transitively, which by default registers the
// process as a regular foreground app (Dock icon, menu bar). `.prohibited`
// makes it a true background helper. Must run before any AppKit API touches
// process activation state, so it goes at the top of main.
NSApplication.shared.setActivationPolicy(.prohibited)

// MARK: - Mode dispatch

let allArgs = CommandLine.arguments

func micStatusString() -> String {
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized: return "granted"
    case .denied: return "denied"
    case .restricted: return "restricted"
    case .notDetermined: return "not_determined"
    @unknown default: return "unknown"
    }
}

func screenStatusString() -> String {
    // Some unsigned/dev binaries can have CGPreflightScreenCaptureAccess block
    // indefinitely on TCC lookup. Race it against a short watchdog so `status`
    // never hangs and the permissions UI stays responsive.
    let sema = DispatchSemaphore(value: 0)
    var result = "unknown"
    DispatchQueue.global(qos: .userInitiated).async {
        let granted = CGPreflightScreenCaptureAccess()
        result = granted ? "granted" : "denied"
        sema.signal()
    }
    if sema.wait(timeout: .now() + .milliseconds(800)) == .timedOut {
        return "unknown"
    }
    return result
}

func printJSON(_ obj: [String: Any]) {
    if let data = try? JSONSerialization.data(withJSONObject: obj),
       let s = String(data: data, encoding: .utf8) {
        print(s)
        fflush(stdout)
    }
}

if allArgs.count >= 2 {
    switch allArgs[1] {
    case "status":
        printJSON([
            "microphone": micStatusString(),
            "screen": screenStatusString(),
        ])
        exit(0)
    case "request-microphone":
        AVCaptureDevice.requestAccess(for: .audio) { granted in
            printJSON(["microphone": granted ? "granted" : "denied"])
            exit(granted ? 0 : 1)
        }
        RunLoop.main.run()
        exit(1) // unreachable
    case "request-screen":
        // CGRequestScreenCaptureAccess returns true if already granted; otherwise it adds
        // the app to the privacy pane and returns false. The user must enable it manually
        // and the app must be relaunched for the new permission to take effect.
        let ok = CGRequestScreenCaptureAccess()
        printJSON(["screen": ok ? "granted" : "denied"])
        exit(ok ? 0 : 1)
    default:
        break
    }
}

// MARK: - Args (recording mode)

var outDir = FileManager.default.temporaryDirectory
let args = CommandLine.arguments
if let i = args.firstIndex(of: "--out"), i + 1 < args.count {
    outDir = URL(fileURLWithPath: args[i + 1])
}
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true, attributes: nil)

// MARK: - JSON event emitter (stdout)

let stdoutLock = NSLock()
func emit(_ obj: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: obj),
          let line = String(data: data, encoding: .utf8) else { return }
    stdoutLock.lock()
    print(line)
    fflush(stdout)
    stdoutLock.unlock()
}

func emitError(_ msg: String) { emit(["event": "error", "message": msg]) }

// MARK: - Target format: 16 kHz mono Float32 (in memory) → Int16 WAV on disk

let targetSampleRate: Double = 16_000
let targetFormat = AVAudioFormat(
    commonFormat: .pcmFormatFloat32,
    sampleRate: targetSampleRate,
    channels: 1,
    interleaved: false
)!
let writeSettings: [String: Any] = [
    AVFormatIDKey: kAudioFormatLinearPCM,
    AVSampleRateKey: targetSampleRate,
    AVNumberOfChannelsKey: 1,
    AVLinearPCMBitDepthKey: 16,
    AVLinearPCMIsFloatKey: false,
    AVLinearPCMIsBigEndianKey: false,
]

// MARK: - Chunk writer (per source)

// One ChunkWriter per source (mic / sys). Each writes its own VAD-bounded
// chunk WAVs and tags every emitted event with the `source` so the Rust side
// can route transcribes and label the final transcript ("You" for mic, the
// diarized speaker IDs for system). Filenames are prefixed by source so the
// two writers can share the same temp dir without colliding.
final class ChunkWriter {
    private let source: String
    private let dir: URL
    private let minFrames: AVAudioFrameCount
    private let maxFrames: AVAudioFrameCount
    private let vadSilenceFrames: AVAudioFrameCount
    private let silenceThreshold: Float = 0.005   // chunk-level: below this we drop the chunk
    private let vadFrameThreshold: Float = 0.008  // per-buffer peak: above this counts as voice
    private var index: Int = 0
    private var file: AVAudioFile?
    private var url: URL?
    private var written: AVAudioFrameCount = 0
    private var chunkPeak: Float = 0
    private var silentRun: AVAudioFrameCount = 0
    // Total frames written across ALL chunks since the writer opened. Used
    // to compute each chunk's start_ms relative to this stream's t=0 (the
    // first frame this writer ever received). Each stream has its own
    // timeline; the offline diarize pass aligns chunks within their own
    // full.wav, so per-stream-relative is the right anchor.
    private var totalFramesWritten: AVAudioFrameCount = 0
    private var chunkStartFrames: AVAudioFrameCount = 0
    private let queue: DispatchQueue

    init(source: String, dir: URL, minSeconds: Double, maxSeconds: Double, vadSilenceMs: Double) {
        self.source = source
        self.dir = dir
        self.minFrames = AVAudioFrameCount(minSeconds * targetSampleRate)
        self.maxFrames = AVAudioFrameCount(maxSeconds * targetSampleRate)
        self.vadSilenceFrames = AVAudioFrameCount((vadSilenceMs / 1000.0) * targetSampleRate)
        self.queue = DispatchQueue(label: "chunk.writer.\(source)")
    }

    func write(_ buffer: AVAudioPCMBuffer) {
        queue.sync {
            do {
                if file == nil { try openNext() }
                try file!.write(from: buffer)
                written += buffer.frameLength
                totalFramesWritten += buffer.frameLength

                // Per-buffer peak feeds both the chunk-level peak (used for the
                // silence-drop on close) and the silent-run counter (used by
                // the VAD rotation trigger).
                var bufPeak: Float = 0
                if let chans = buffer.floatChannelData {
                    let n = Int(buffer.frameLength)
                    for i in 0..<n {
                        let v = abs(chans[0][i])
                        if v > bufPeak { bufPeak = v }
                    }
                }
                if bufPeak > chunkPeak { chunkPeak = bufPeak }
                if bufPeak < vadFrameThreshold {
                    silentRun += buffer.frameLength
                } else {
                    silentRun = 0
                }

                // Rotate on whichever fires first:
                //  - hard cap (maxFrames) so a continuous monologue still gets
                //    transcribed periodically and the trailing-context prompt
                //    stays fresh on the consuming side.
                //  - VAD pause detected, but only after the chunk reached
                //    minFrames so we don't emit micro-chunks that lose context.
                let vadRotate = written >= minFrames && silentRun >= vadSilenceFrames
                if written >= maxFrames || vadRotate {
                    try rotate()
                }
            } catch {
                emitError("\(source) write: \(error.localizedDescription)")
            }
        }
    }

    func close() {
        queue.sync {
            if let u = url, written > 0 {
                file = nil
                if chunkPeak >= silenceThreshold {
                    let startMs = Int(Double(chunkStartFrames) / targetSampleRate * 1000.0)
                    emit([
                        "event": "chunk",
                        "source": source,
                        "path": u.path,
                        "start_ms": startMs,
                    ])
                    stats.lock.lock(); stats.chunks += 1; stats.lock.unlock()
                } else {
                    try? FileManager.default.removeItem(at: u)
                }
            }
            file = nil
            url = nil
            written = 0
            chunkPeak = 0
            silentRun = 0
        }
    }

    private func openNext() throws {
        index += 1
        let u = dir.appendingPathComponent(String(format: "%@-chunk-%04d.wav", source, index))
        url = u
        file = try AVAudioFile(forWriting: u, settings: writeSettings)
        written = 0
        chunkStartFrames = totalFramesWritten
    }

    private func rotate() throws {
        guard let u = url else { return }
        file = nil
        if chunkPeak >= silenceThreshold {
            let startMs = Int(Double(chunkStartFrames) / targetSampleRate * 1000.0)
            emit([
                "event": "chunk",
                "source": source,
                "path": u.path,
                "start_ms": startMs,
            ])
            stats.lock.lock(); stats.chunks += 1; stats.lock.unlock()
        } else {
            try? FileManager.default.removeItem(at: u)
        }
        chunkPeak = 0
        silentRun = 0
        try openNext()
    }
}

// Tuned to keep VAD as the primary boundary picker, with the max only as
// a safety net:
//   - minSeconds 1.0 lets short utterances flush quickly.
//   - maxSeconds 15.0 — high enough that an 8 s monologue doesn't cap
//     mid-word ("mistenkte" → "mistred"). Whisper actually transcribes
//     longer chunks more accurately because it sees more context, so we
//     prefer letting VAD pick the boundary even if that's a bit slower.
//   - vadSilenceMs 500 catches sentence-end pauses without triggering on
//     normal between-word stops (which are typically 100–300 ms).
let micWriter = ChunkWriter(source: "mic", dir: outDir, minSeconds: 1.0, maxSeconds: 15.0, vadSilenceMs: 500.0)
let sysWriter = ChunkWriter(source: "sys", dir: outDir, minSeconds: 1.0, maxSeconds: 15.0, vadSilenceMs: 500.0)

// MARK: - Full-recording writer (per source)

// Parallel writer that captures every received frame into a single WAV for
// the duration of the recording. Each source gets its own full.wav (so the
// post-stop diarizer can treat them as independent streams: in-person calls
// produce only mic_full.wav and run multi-speaker diarize there; remote
// calls produce both files and run "mic = You, sys = diarize speakers").
// ~58 MB per 30-min meeting at 16 kHz mono 16-bit per source.
final class FullRecordingWriter {
    private let source: String
    private let dir: URL
    private var file: AVAudioFile?
    private var url: URL?
    private var written: AVAudioFrameCount = 0
    private let queue: DispatchQueue

    init(source: String, dir: URL) {
        self.source = source
        self.dir = dir
        self.queue = DispatchQueue(label: "full.writer.\(source)")
    }

    func write(_ buffer: AVAudioPCMBuffer) {
        queue.sync {
            do {
                if file == nil {
                    let u = dir.appendingPathComponent("\(source)-full.wav")
                    url = u
                    file = try AVAudioFile(forWriting: u, settings: writeSettings)
                }
                try file!.write(from: buffer)
                written += buffer.frameLength
            } catch {
                emitError("\(source) full write: \(error.localizedDescription)")
            }
        }
    }

    func close() {
        queue.sync {
            file = nil
            if let u = url, written > 0 {
                let durationMs = Int(Double(written) / targetSampleRate * 1000.0)
                emit([
                    "event": "full_recording",
                    "source": source,
                    "path": u.path,
                    "duration_ms": durationMs,
                ])
            }
            url = nil
            written = 0
        }
    }
}

let micFullWriter = FullRecordingWriter(source: "mic", dir: outDir)
let sysFullWriter = FullRecordingWriter(source: "sys", dir: outDir)

// MARK: - Stats (diagnostics)

final class Stats {
    let lock = NSLock()
    var micFrames: Int = 0
    var sysFrames: Int = 0
    var chunks: Int = 0
    var micPeak: Float = 0
    var sysPeak: Float = 0
}
let stats = Stats()

func recordMicStats(samples: [Float]) {
    let peak = samples.reduce(0 as Float) { max($0, abs($1)) }
    stats.lock.lock()
    stats.micFrames += samples.count
    if peak > stats.micPeak { stats.micPeak = peak }
    stats.lock.unlock()
}

func recordSysStats(samples: [Float]) {
    let peak = samples.reduce(0 as Float) { max($0, abs($1)) }
    stats.lock.lock()
    stats.sysFrames += samples.count
    if peak > stats.sysPeak { stats.sysPeak = peak }
    stats.lock.unlock()
}

// Wrap a Float32 sample array into an AVAudioPCMBuffer for the writers. The
// writers expect mono Float32 at the target sample rate.
func makeBuffer(_ samples: [Float]) -> AVAudioPCMBuffer? {
    guard !samples.isEmpty,
          let buf = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: AVAudioFrameCount(samples.count)) else {
        return nil
    }
    buf.frameLength = AVAudioFrameCount(samples.count)
    if let chans = buf.floatChannelData {
        samples.withUnsafeBufferPointer { src in
            chans[0].update(from: src.baseAddress!, count: samples.count)
        }
    }
    return buf
}

// MARK: - Mic via AVAudioEngine

let engine = AVAudioEngine()
var micConverter: AVAudioConverter?
do {
    let input = engine.inputNode
    let inFormat = input.inputFormat(forBus: 0)
    if inFormat.sampleRate == 0 || inFormat.channelCount == 0 {
        emitError("Microphone input format invalid (sampleRate=\(inFormat.sampleRate), channels=\(inFormat.channelCount)). Dev binaries without an Info.plist may be silently denied audio. Try running 'pnpm tauri build --debug' and launching the .app instead of 'pnpm tauri dev'.")
        throw NSError(domain: "audio-capture", code: 1, userInfo: [NSLocalizedDescriptionKey: "invalid input format"])
    }
    micConverter = AVAudioConverter(from: inFormat, to: targetFormat)
    input.installTap(onBus: 0, bufferSize: 4096, format: inFormat) { buffer, _ in
        guard let conv = micConverter else { return }
        let ratio = targetSampleRate / inFormat.sampleRate
        let cap = AVAudioFrameCount(Double(buffer.frameLength) * ratio + 1024)
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: cap) else { return }
        var error: NSError?
        var supplied = false
        let status = conv.convert(to: out, error: &error) { _, status in
            if supplied {
                status.pointee = .noDataNow
                return nil
            }
            supplied = true
            status.pointee = .haveData
            return buffer
        }
        if status != .error, out.frameLength > 0,
           let chans = out.floatChannelData {
            let n = Int(out.frameLength)
            let arr = Array(UnsafeBufferPointer(start: chans[0], count: n))
            recordMicStats(samples: arr)
            if let buf = makeBuffer(arr) {
                micWriter.write(buf)
                micFullWriter.write(buf)
            }
        }
    }
    engine.prepare()
    try engine.start()
} catch {
    emitError("mic engine: \(error.localizedDescription)")
}

// MARK: - System audio via ScreenCaptureKit

final class SystemAudioOutput: NSObject, SCStreamOutput {
    var converter: AVAudioConverter?
    var inFormat: AVAudioFormat?

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }
        guard CMSampleBufferIsValid(sampleBuffer),
              let formatDesc = CMSampleBufferGetFormatDescription(sampleBuffer),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc)?.pointee
        else { return }

        // Build/refresh source format
        if inFormat == nil || inFormat?.sampleRate != asbd.mSampleRate ||
            inFormat?.channelCount != asbd.mChannelsPerFrame {
            var asbdCopy = asbd
            inFormat = AVAudioFormat(streamDescription: &asbdCopy)
            if let inF = inFormat {
                converter = AVAudioConverter(from: inF, to: targetFormat)
            }
        }
        guard let inFormat = inFormat, let conv = converter else { return }

        // CMSampleBuffer → AVAudioPCMBuffer
        let frames = AVAudioFrameCount(CMSampleBufferGetNumSamples(sampleBuffer))
        guard frames > 0,
              let inBuffer = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: frames) else { return }
        inBuffer.frameLength = frames

        var blockBuffer: CMBlockBuffer?
        var audioBufferList = AudioBufferList()
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer,
            bufferListSizeNeededOut: nil,
            bufferListOut: &audioBufferList,
            bufferListSize: MemoryLayout<AudioBufferList>.size,
            blockBufferAllocator: nil,
            blockBufferMemoryAllocator: nil,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard status == noErr else { return }

        let abl = UnsafeMutableAudioBufferListPointer(&audioBufferList)
        // Copy interleaved/non-interleaved input into inBuffer.
        if let dst = inBuffer.mutableAudioBufferList.pointee.mBuffers.mData,
           let src = abl[0].mData {
            let n = Int(min(abl[0].mDataByteSize, inBuffer.mutableAudioBufferList.pointee.mBuffers.mDataByteSize))
            memcpy(dst, src, n)
        }

        let ratio = targetSampleRate / inFormat.sampleRate
        let cap = AVAudioFrameCount(Double(frames) * ratio + 1024)
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: cap) else { return }

        var error: NSError?
        var supplied = false
        let convStatus = conv.convert(to: out, error: &error) { _, status in
            if supplied { status.pointee = .noDataNow; return nil }
            supplied = true
            status.pointee = .haveData
            return inBuffer
        }
        guard convStatus != .error, let chans = out.floatChannelData else { return }
        let n = Int(out.frameLength)
        let arr = Array(UnsafeBufferPointer(start: chans[0], count: n))
        recordSysStats(samples: arr)
        if let buf = makeBuffer(arr) {
            sysWriter.write(buf)
            sysFullWriter.write(buf)
        }
    }
}

let systemOutput = SystemAudioOutput()
var scStream: SCStream?

func startSystemAudio() async {
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first else {
            emitError("no display")
            return
        }
        // Filter excludes our own process so we don't capture our own output.
        let filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])

        let cfg = SCStreamConfiguration()
        cfg.capturesAudio = true
        cfg.excludesCurrentProcessAudio = true
        cfg.sampleRate = 48_000
        cfg.channelCount = 2
        // Minimize video work; we still need a video stream for SCK to be happy.
        cfg.width = 2
        cfg.height = 2
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        cfg.queueDepth = 5

        let stream = SCStream(filter: filter, configuration: cfg, delegate: nil)
        let q = DispatchQueue(label: "sck.audio")
        try stream.addStreamOutput(systemOutput, type: .audio, sampleHandlerQueue: q)
        // Adding a video output is required by SCK; we just discard.
        try stream.addStreamOutput(NoopVideoOutput(), type: .screen, sampleHandlerQueue: DispatchQueue(label: "sck.video"))
        try await stream.startCapture()
        scStream = stream
    } catch {
        emitError("screen capture: \(error.localizedDescription)")
    }
}

final class NoopVideoOutput: NSObject, SCStreamOutput {
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {}
}

Task { await startSystemAudio() }

// MARK: - Heartbeat (every 2s) for live diagnostics

let hbQueue = DispatchQueue(label: "heartbeat")
let hbTimer = DispatchSource.makeTimerSource(queue: hbQueue)
hbTimer.schedule(deadline: .now() + 2, repeating: 2)
hbTimer.setEventHandler {
    stats.lock.lock()
    let mF = stats.micFrames
    let sF = stats.sysFrames
    let ch = stats.chunks
    let mp = stats.micPeak
    let sp = stats.sysPeak
    stats.micPeak = 0
    stats.sysPeak = 0
    stats.lock.unlock()
    emit([
        "event": "heartbeat",
        "mic_frames": mF,
        "sys_frames": sF,
        "chunks": ch,
        "mic_peak": mp,
        "sys_peak": sp,
    ])
}
hbTimer.resume()

// MARK: - Pause / Resume via SIGUSR1 / SIGUSR2

var paused: Bool = false

func pauseCapture() {
    if paused { return }
    paused = true
    engine.pause()
    if let s = scStream {
        Task { try? await s.stopCapture() }
        scStream = nil
    }
    emit(["event": "paused"])
}

func resumeCapture() {
    if !paused { return }
    paused = false
    do {
        try engine.start()
    } catch {
        emitError("resume mic: \(error.localizedDescription)")
    }
    Task { await startSystemAudio() }
    emit(["event": "resumed"])
}

let pauseSrc = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
let resumeSrc = DispatchSource.makeSignalSource(signal: SIGUSR2, queue: .main)
signal(SIGUSR1, SIG_IGN)
signal(SIGUSR2, SIG_IGN)
pauseSrc.setEventHandler { pauseCapture() }
resumeSrc.setEventHandler { resumeCapture() }
pauseSrc.resume()
resumeSrc.resume()

// MARK: - Parent-death watchdog
//
// We `setsid()` from the Rust side so this sidecar gets its own session
// (necessary for TCC permissions to bind to the *sidecar's* binary identity
// rather than the parent's). A side effect of detached sessions is that the
// process survives parent death — the launching app crashing, a `pnpm tauri
// dev` reload, or a force-quit leaves an orphan running indefinitely. macOS
// will reparent it to launchd (PID 1) in those cases.
//
// Poll PPID every 2 s. If we see PID 1 as our parent, the launcher is gone
// and we should exit so the next launch starts cleanly without a zombie
// sidecar holding onto the mic.
let originalParentPid = getppid()
let parentWatchdog = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "parent.watchdog"))
parentWatchdog.schedule(deadline: .now() + 2, repeating: 2)
parentWatchdog.setEventHandler {
    let current = getppid()
    if current == 1 || (originalParentPid != 1 && current != originalParentPid) {
        exit(0)
    }
}
parentWatchdog.resume()

// MARK: - Signal handling: SIGTERM / SIGINT → finalize

let sigSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
let sigSource2 = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
signal(SIGTERM, SIG_IGN)
signal(SIGINT, SIG_IGN)

let shutdown: () -> Void = {
    Task {
        if let s = scStream {
            try? await s.stopCapture()
        }
        engine.stop()
        // Order matters: emit `full_recording` events BEFORE `stopped` so the
        // Rust reader sees both full WAV paths before its loop breaks out on
        // `stopped`. Each ChunkWriter.close() emits its final chunk (if any);
        // the single `stopped` event signals end-of-stream for the entire
        // sidecar (both sources finished).
        micFullWriter.close()
        sysFullWriter.close()
        micWriter.close()
        sysWriter.close()
        emit(["event": "stopped"])
        exit(0)
    }
}

sigSource.setEventHandler(handler: shutdown)
sigSource2.setEventHandler(handler: shutdown)
sigSource.resume()
sigSource2.resume()

RunLoop.main.run()
