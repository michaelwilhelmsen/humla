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

// Import mode: `--import <file>`. Instead of live-capturing mic + system audio,
// decode an existing file, resample it to the target format, and replay it
// through the SAME VAD ChunkWriter + FullRecordingWriter used for the mic
// stream — emitting the identical `chunk` / `full_recording` / `stopped`
// events. The Rust side then reuses its whole recording pipeline unchanged
// (transcribe fan-out → mic-only diarize). A one-shot replay: no AVAudioEngine,
// no ScreenCaptureKit, no heartbeat / pause / watchdog / signal handlers.
let importPath: String? = {
    guard let i = args.firstIndex(of: "--import"), i + 1 < args.count else { return nil }
    return args[i + 1]
}()
let isImport = importPath != nil

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

// Build the mic converter for `inFormat` and install the capture tap. Factored
// out so both initial setup and post-device-change recovery install an
// identical tap against whatever the current input format happens to be.
func installMicTap(_ input: AVAudioInputNode, format inFormat: AVAudioFormat) {
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
}

if !isImport {
do {
    let input = engine.inputNode
    // NOTE: do NOT call `input.setVoiceProcessingEnabled(true)`. It enables
    // hardware AEC, but the voice-processing IO unit takes over the audio
    // device for *both* directions, which ducks the system output (you
    // can't hear the other person on a call). Echo-cancel the mic in a
    // post-stop dedup pass instead — see commands.rs.
    let inFormat = input.inputFormat(forBus: 0)
    if inFormat.sampleRate == 0 || inFormat.channelCount == 0 {
        emitError("Microphone input format invalid (sampleRate=\(inFormat.sampleRate), channels=\(inFormat.channelCount)). Dev binaries without an Info.plist may be silently denied audio. Try running 'pnpm tauri build --debug' and launching the .app instead of 'pnpm tauri dev'.")
        throw NSError(domain: "audio-capture", code: 1, userInfo: [NSLocalizedDescriptionKey: "invalid input format"])
    }
    installMicTap(input, format: inFormat)
    engine.prepare()
    try engine.start()
} catch {
    emitError("mic engine: \(error.localizedDescription)")
}
} // if !isImport

// MARK: - System audio via ScreenCaptureKit

final class StreamDelegate: NSObject, SCStreamDelegate {
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        emitError("screen capture stopped: \(error.localizedDescription)")
    }
    func outputVideoEffectDidStart(for stream: SCStream) {
        FileHandle.standardError.write(Data("scstream: video effect started\n".utf8))
    }
    func outputVideoEffectDidStop(for stream: SCStream) {
        FileHandle.standardError.write(Data("scstream: video effect stopped\n".utf8))
    }
}

let streamDelegate = StreamDelegate()

final class SystemAudioOutput: NSObject, SCStreamOutput {
    var converter: AVAudioConverter?
    var inFormat: AVAudioFormat?
    var bufferCount: Int = 0

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }
        bufferCount += 1
        if bufferCount == 1 {
            FileHandle.standardError.write(Data("scstream: first audio buffer received\n".utf8))
        }
        guard CMSampleBufferIsValid(sampleBuffer),
              let formatDesc = CMSampleBufferGetFormatDescription(sampleBuffer),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc)?.pointee
        else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data("scstream: invalid audio buffer (count=\(bufferCount))\n".utf8))
            }
            return
        }
        if bufferCount == 1 {
            FileHandle.standardError.write(Data(
                "scstream: input format sr=\(asbd.mSampleRate)Hz channels=\(asbd.mChannelsPerFrame) bytes/frame=\(asbd.mBytesPerFrame) flags=\(asbd.mFormatFlags)\n".utf8
            ))
        }

        // Build/refresh source format
        if inFormat == nil || inFormat?.sampleRate != asbd.mSampleRate ||
            inFormat?.channelCount != asbd.mChannelsPerFrame {
            var asbdCopy = asbd
            inFormat = AVAudioFormat(streamDescription: &asbdCopy)
            if let inF = inFormat {
                converter = AVAudioConverter(from: inF, to: targetFormat)
                FileHandle.standardError.write(Data(
                    "scstream: built converter inFormat=\(inF) → target\n".utf8
                ))
            } else {
                FileHandle.standardError.write(Data(
                    "scstream: AVAudioFormat(streamDescription:) returned nil\n".utf8
                ))
            }
        }
        guard let inFormat = inFormat, let conv = converter else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data(
                    "scstream: bail at converter check (inFormat=\(self.inFormat as Any), conv=\(self.converter as Any))\n".utf8
                ))
            }
            return
        }

        // CMSampleBuffer → AVAudioPCMBuffer
        let frames = AVAudioFrameCount(CMSampleBufferGetNumSamples(sampleBuffer))
        guard frames > 0 else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data("scstream: zero frames\n".utf8))
            }
            return
        }
        guard let inBuffer = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: frames) else {
            FileHandle.standardError.write(Data("scstream: AVAudioPCMBuffer alloc failed\n".utf8))
            return
        }
        inBuffer.frameLength = frames

        // SCK delivers deinterleaved Float32 with up to N channels (2 for
        // stereo system audio). The default `AudioBufferList()` only holds
        // ONE AudioBuffer slot; CMSampleBufferGetAudioBufferListWith… needs
        // a slot per channel and returns -12737
        // (kCMSampleBufferError_ArrayTooSmall) if the list is too small.
        // Allocate dynamically based on channel count.
        var blockBuffer: CMBlockBuffer?
        let numBuffers = max(1, Int(inFormat.channelCount))
        let listSize = MemoryLayout<AudioBufferList>.size
            + max(0, numBuffers - 1) * MemoryLayout<AudioBuffer>.size
        let abl = AudioBufferList.allocate(maximumBuffers: numBuffers)
        defer { free(abl.unsafeMutablePointer) }

        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer,
            bufferListSizeNeededOut: nil,
            bufferListOut: abl.unsafeMutablePointer,
            bufferListSize: listSize,
            blockBufferAllocator: nil,
            blockBufferMemoryAllocator: nil,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
            blockBufferOut: &blockBuffer
        )
        guard status == noErr else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data("scstream: GetAudioBufferList status=\(status) (numBuffers=\(numBuffers), listSize=\(listSize))\n".utf8))
            }
            return
        }

        // Copy each channel from the source list into the matching slot in
        // inBuffer's deinterleaved layout.
        let inAbl = UnsafeMutableAudioBufferListPointer(inBuffer.mutableAudioBufferList)
        let copyChannels = min(inAbl.count, abl.count)
        for i in 0..<copyChannels {
            if let dst = inAbl[i].mData, let src = abl[i].mData {
                let n = Int(min(abl[i].mDataByteSize, inAbl[i].mDataByteSize))
                memcpy(dst, src, n)
            }
        }

        let ratio = targetSampleRate / inFormat.sampleRate
        let cap = AVAudioFrameCount(Double(frames) * ratio + 1024)
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: cap) else {
            FileHandle.standardError.write(Data("scstream: out PCMBuffer alloc failed\n".utf8))
            return
        }

        var error: NSError?
        var supplied = false
        let convStatus = conv.convert(to: out, error: &error) { _, status in
            if supplied { status.pointee = .noDataNow; return nil }
            supplied = true
            status.pointee = .haveData
            return inBuffer
        }
        if convStatus == .error {
            if bufferCount <= 3 {
                let msg = error?.localizedDescription ?? "unknown"
                FileHandle.standardError.write(Data("scstream: conv.convert error: \(msg)\n".utf8))
            }
            return
        }
        guard let chans = out.floatChannelData else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data("scstream: no floatChannelData on out buffer\n".utf8))
            }
            return
        }
        let n = Int(out.frameLength)
        let arr = Array(UnsafeBufferPointer(start: chans[0], count: n))
        recordSysStats(samples: arr)
        if let buf = makeBuffer(arr) {
            sysWriter.write(buf)
            sysFullWriter.write(buf)
            if bufferCount == 1 {
                FileHandle.standardError.write(Data(
                    "scstream: first buffer written to sysWriter (n=\(n) samples)\n".utf8
                ))
            }
        } else {
            if bufferCount <= 3 {
                FileHandle.standardError.write(Data("scstream: makeBuffer returned nil (n=\(n))\n".utf8))
            }
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

        let stream = SCStream(filter: filter, configuration: cfg, delegate: streamDelegate)
        FileHandle.standardError.write(Data("scstream: starting capture (display=\(display.displayID))\n".utf8))
        let q = DispatchQueue(label: "sck.audio")
        try stream.addStreamOutput(systemOutput, type: .audio, sampleHandlerQueue: q)
        // Adding a video output is required by SCK; we just discard.
        try stream.addStreamOutput(NoopVideoOutput(), type: .screen, sampleHandlerQueue: DispatchQueue(label: "sck.video"))
        try await stream.startCapture()
        scStream = stream
        FileHandle.standardError.write(Data("scstream: capture started successfully\n".utf8))
    } catch {
        emitError("screen capture: \(error.localizedDescription)")
    }
}

final class NoopVideoOutput: NSObject, SCStreamOutput {
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {}
}

if !isImport {
    Task { await startSystemAudio() }
}

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
if !isImport {
    hbTimer.resume()
}

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
if !isImport {
    pauseSrc.resume()
    resumeSrc.resume()
}

// MARK: - Audio device / configuration change recovery
//
// Re-establish mic capture after an audio device or format change mid-recording
// (e.g. plugging in HDMI, a USB interface, or AirPods). On a configuration
// change AVAudioEngine stops itself and the input format can change out from
// under the existing tap; without re-reading the format, re-tapping, and
// restarting, the mic tap silently stops firing for the rest of the session
// while ScreenCaptureKit (system audio) keeps running — the failure that
// produced a 3-minute mic stream alongside an 85-minute system stream. The
// notification's object is our engine; `.main` delivery serialises recovery
// with pause/resume/shutdown.
func reconfigureMicAfterDeviceChange() {
    let input = engine.inputNode
    input.removeTap(onBus: 0)
    let newFormat = input.inputFormat(forBus: 0)
    guard newFormat.sampleRate > 0, newFormat.channelCount > 0 else {
        emitError("Audio device changed but the new microphone format is unavailable; mic capture did not resume.")
        return
    }
    installMicTap(input, format: newFormat)
    // While paused the engine is intentionally stopped — resumeCapture() starts
    // it later, now against the freshly-installed tap. Only restart here if we
    // were actively recording when the device changed.
    if !paused {
        do {
            engine.prepare()
            try engine.start()
        } catch {
            emitError("Failed to restart microphone after device change: \(error.localizedDescription)")
            return
        }
    }
    emit(["event": "diagnostic", "message": "Audio input device changed; microphone capture resumed on the new device."])
}

if !isImport {
    NotificationCenter.default.addObserver(
        forName: .AVAudioEngineConfigurationChange,
        object: engine,
        queue: .main
    ) { _ in
        reconfigureMicAfterDeviceChange()
    }
}

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
if !isImport {
    parentWatchdog.resume()
}

// MARK: - Signal handling: SIGTERM / SIGINT → finalize

let sigSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
let sigSource2 = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
signal(SIGTERM, SIG_IGN)
signal(SIGINT, SIG_IGN)

let shutdown: () -> Void = {
    Task {
        // Close all writers FIRST, before any async cleanup that
        // might block. ScreenCaptureKit's `stopCapture()` has been
        // observed to take multiple seconds — if we await it before
        // closing the writers, the parent's SIGTERM grace window can
        // expire and SIGKILL truncates this process before any
        // `chunk` / `full_recording` / `stopped` events reach
        // stdout. That manifests on the user's side as "the last
        // 20–30 s of audio never gets transcribed" because the
        // chunks for that tail were ready to emit but never did.
        // engine.stop() is synchronous and returns immediately;
        // ChunkWriter.close() is also synchronous (queue.sync).
        engine.stop()
        micFullWriter.close()
        sysFullWriter.close()
        micWriter.close()
        sysWriter.close()
        emit(["event": "stopped"])
        // Now best-effort SCK shutdown for cleanliness. If it stalls,
        // we've already emitted everything the parent needs and the
        // OS will reclaim resources on exit.
        if let s = scStream {
            try? await s.stopCapture()
        }
        exit(0)
    }
}

sigSource.setEventHandler(handler: shutdown)
sigSource2.setEventHandler(handler: shutdown)
if !isImport {
    sigSource.resume()
    sigSource2.resume()
}

// MARK: - Import replay
//
// Decode the source file to PCM, resample each block to the target 16 kHz
// mono format via AVAudioConverter, and feed the SAME mic writers as live
// capture. Runs at full speed (no realtime pacing) — the OS/disk buffers the
// per-chunk WAVs and the Rust reader applies a bounded-backlog semaphore so a
// fast replay can't swamp the transcribe queue. On completion it closes the
// writers (emitting the final chunk + `full_recording`), emits `stopped`, and
// exits. Only the mic writers are touched, so the Rust side sees a mic-only
// session and runs the mic-only diarize branch — exactly what we want.
func runImport(_ path: String) {
    let url = URL(fileURLWithPath: path)
    let file: AVAudioFile
    do {
        file = try AVAudioFile(forReading: url)
    } catch {
        emitError("import open \(url.lastPathComponent): \(error.localizedDescription)")
        emit(["event": "stopped"])
        exit(1)
    }

    let inFormat = file.processingFormat
    guard let converter = AVAudioConverter(from: inFormat, to: targetFormat) else {
        emitError("import: could not build converter from \(inFormat)")
        emit(["event": "stopped"])
        exit(1)
    }

    // Read/convert in ~1 s blocks. Block size is at the source rate; the
    // converter downmixes to mono and resamples to 16 kHz in one pass.
    let blockFrames = AVAudioFrameCount(max(inFormat.sampleRate, 16_000))
    while true {
        guard let inBuf = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: blockFrames) else { break }
        do {
            try file.read(into: inBuf)
        } catch {
            emitError("import read: \(error.localizedDescription)")
            break
        }
        if inBuf.frameLength == 0 { break } // EOF

        let ratio = targetSampleRate / inFormat.sampleRate
        let cap = AVAudioFrameCount(Double(inBuf.frameLength) * ratio + 1024)
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: cap) else { break }
        var convErr: NSError?
        var supplied = false
        let status = converter.convert(to: out, error: &convErr) { _, s in
            if supplied { s.pointee = .noDataNow; return nil }
            supplied = true
            s.pointee = .haveData
            return inBuf
        }
        if status == .error {
            emitError("import convert: \(convErr?.localizedDescription ?? "unknown")")
            break
        }
        if out.frameLength > 0, let chans = out.floatChannelData {
            let n = Int(out.frameLength)
            let arr = Array(UnsafeBufferPointer(start: chans[0], count: n))
            if let buf = makeBuffer(arr) {
                micWriter.write(buf)
                micFullWriter.write(buf)
            }
        }
    }

    micWriter.close()
    micFullWriter.close()
    emit(["event": "stopped"])
    exit(0)
}

if let importPath = importPath {
    runImport(importPath)
} else {
    RunLoop.main.run()
}
