import Foundation
import AVFoundation
import ScreenCaptureKit
import CoreMedia
import CoreGraphics

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

// MARK: - Chunk writer

final class ChunkWriter {
    private let dir: URL
    private let chunkFrames: AVAudioFrameCount
    private var index: Int = 0
    private var file: AVAudioFile?
    private var url: URL?
    private var written: AVAudioFrameCount = 0
    private var chunkPeak: Float = 0
    private let queue = DispatchQueue(label: "chunk.writer")
    private let silenceThreshold: Float = 0.005  // ~-46dB; below this Whisper hallucinates

    init(dir: URL, chunkSeconds: Double) {
        self.dir = dir
        self.chunkFrames = AVAudioFrameCount(chunkSeconds * targetSampleRate)
    }

    func write(_ buffer: AVAudioPCMBuffer) {
        queue.sync {
            do {
                if file == nil { try openNext() }
                try file!.write(from: buffer)
                written += buffer.frameLength
                if let chans = buffer.floatChannelData {
                    let n = Int(buffer.frameLength)
                    var peak: Float = 0
                    for i in 0..<n {
                        let v = abs(chans[0][i])
                        if v > peak { peak = v }
                    }
                    if peak > chunkPeak { chunkPeak = peak }
                }
                if written >= chunkFrames {
                    try rotate()
                }
            } catch {
                emitError("write: \(error.localizedDescription)")
            }
        }
    }

    func close() {
        queue.sync {
            if let u = url, written > 0 {
                file = nil
                if chunkPeak >= silenceThreshold {
                    emit(["event": "chunk", "path": u.path])
                    stats.lock.lock(); stats.chunks += 1; stats.lock.unlock()
                } else {
                    try? FileManager.default.removeItem(at: u)
                }
            }
            file = nil
            url = nil
            written = 0
            chunkPeak = 0
            emit(["event": "stopped"])
        }
    }

    private func openNext() throws {
        index += 1
        let u = dir.appendingPathComponent(String(format: "chunk-%04d.wav", index))
        url = u
        file = try AVAudioFile(forWriting: u, settings: writeSettings)
        written = 0
    }

    private func rotate() throws {
        guard let u = url else { return }
        file = nil
        if chunkPeak >= silenceThreshold {
            emit(["event": "chunk", "path": u.path])
            stats.lock.lock(); stats.chunks += 1; stats.lock.unlock()
        } else {
            try? FileManager.default.removeItem(at: u)
        }
        chunkPeak = 0
        try openNext()
    }
}

let writer = ChunkWriter(dir: outDir, chunkSeconds: 20.0)

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

// MARK: - Mixing buffer

/// A simple lock-protected sliding mix buffer. Each source writes Float32 samples
/// at the target rate; on timer tick, we take the prefix that both sources have
/// reached, mix them sample-wise, and emit. Slight drift is tolerated.
final class Mixer {
    private let lock = NSLock()
    private var mic: [Float] = []
    private var sys: [Float] = []
    private(set) var mixed: [Float] = []

    func push(mic samples: [Float]) {
        let peak = samples.reduce(0 as Float) { max($0, abs($1)) }
        lock.lock()
        mic.append(contentsOf: samples)
        lock.unlock()
        stats.lock.lock()
        stats.micFrames += samples.count
        if peak > stats.micPeak { stats.micPeak = peak }
        stats.lock.unlock()
    }
    func push(sys samples: [Float]) {
        let peak = samples.reduce(0 as Float) { max($0, abs($1)) }
        lock.lock()
        sys.append(contentsOf: samples)
        lock.unlock()
        stats.lock.lock()
        stats.sysFrames += samples.count
        if peak > stats.sysPeak { stats.sysPeak = peak }
        stats.lock.unlock()
    }

    /// Returns up to `maxFrames` mixed samples. If only one source has data,
    /// passes through that source. Caller drives the consumption pace.
    func drain(maxFrames: Int) -> [Float] {
        lock.lock(); defer { lock.unlock() }
        let n = min(maxFrames, max(mic.count, sys.count))
        if n == 0 { return [] }
        var out = [Float](repeating: 0, count: n)
        let mTake = min(mic.count, n)
        let sTake = min(sys.count, n)
        for i in 0..<mTake { out[i] += mic[i] * 0.85 }
        for i in 0..<sTake { out[i] += sys[i] * 0.85 }
        // Soft clip
        for i in 0..<n { out[i] = max(-1.0, min(1.0, out[i])) }
        if mTake > 0 { mic.removeFirst(mTake) }
        if sTake > 0 { sys.removeFirst(sTake) }
        return out
    }
}

let mixer = Mixer()

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
            mixer.push(mic: arr)
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
        mixer.push(sys: arr)
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

// MARK: - Drain timer → write to chunk

let drainQueue = DispatchQueue(label: "drain")
let drainTimer = DispatchSource.makeTimerSource(queue: drainQueue)
drainTimer.schedule(deadline: .now() + .milliseconds(200), repeating: .milliseconds(200))
drainTimer.setEventHandler {
    let frames = mixer.drain(maxFrames: Int(targetSampleRate)) // up to 1s per tick
    if frames.isEmpty { return }
    guard let buf = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: AVAudioFrameCount(frames.count)) else { return }
    buf.frameLength = AVAudioFrameCount(frames.count)
    if let chans = buf.floatChannelData {
        frames.withUnsafeBufferPointer { src in
            chans[0].update(from: src.baseAddress!, count: frames.count)
        }
    }
    writer.write(buf)
}
drainTimer.resume()

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

// MARK: - Signal handling: SIGTERM / SIGINT → finalize

let sigSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
let sigSource2 = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
signal(SIGTERM, SIG_IGN)
signal(SIGINT, SIG_IGN)

let shutdown: () -> Void = {
    drainTimer.cancel()
    if let s = scStream {
        Task {
            try? await s.stopCapture()
            engine.stop()
            // Final drain
            let leftover = mixer.drain(maxFrames: Int(targetSampleRate) * 60)
            if !leftover.isEmpty,
               let buf = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: AVAudioFrameCount(leftover.count)) {
                buf.frameLength = AVAudioFrameCount(leftover.count)
                if let chans = buf.floatChannelData {
                    leftover.withUnsafeBufferPointer { src in
                        chans[0].update(from: src.baseAddress!, count: leftover.count)
                    }
                }
                writer.write(buf)
            }
            writer.close()
            exit(0)
        }
    } else {
        engine.stop()
        writer.close()
        exit(0)
    }
}

sigSource.setEventHandler(handler: shutdown)
sigSource2.setEventHandler(handler: shutdown)
sigSource.resume()
sigSource2.resume()

RunLoop.main.run()
