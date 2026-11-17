import AVFoundation
import CoreAudio
import Foundation

// System audio from a Core Audio process tap (macOS 14.2+), instead of a
// ScreenCaptureKit stream (#220).
//
// Why: on macOS 26 a long ScreenCaptureKit stream can keep delivering audio
// buffers at the normal rate while every sample in them is zero. The stream
// reports no error and no gap, so the far side of a call is silently lost
// for the rest of the take. Seen on eight Zoom calls in four weeks on one
// Mac (sometimes preceded by a stretch where only 1 buffer in 8 carried
// audio); a fresh stream immediately captured audio again. Other SCK-based
// recorders report the same shape (e.g. moona3k/macparakeet#912). A process
// tap reads the processes' output mix from the HAL directly, so it does not
// depend on a ScreenCaptureKit session staying healthy.
//
// Shape: a global stereo tap of every process except Humla's own (this
// sidecar and its parent; the app's WebKit content process is not excluded
// yet, as with ScreenCaptureKit today), a private aggregate device clocked
// off the default output with the tap as its only input, and an IOProc that
// copies each buffer out and hands it to `SystemAudioOutput.ingest`, which
// owns the conversion to 16 kHz mono and the writers — the same path a
// ScreenCaptureKit buffer takes.
//
// A tap's format follows the output device (AirPods joining a call drop to
// 16/24 kHz), and the aggregate is clocked off the device that was default
// when it was built. So the tap and the aggregate are rebuilt whenever the
// default output device or the tap's format changes; `ingest` rebuilds its
// converter when the buffer format changes.
//
// On the one call where the HAL was logged, the ScreenCaptureKit failure
// began within a second of an "Audio IO Overload" — twice, the second time
// on the fresh stream that had recovered it — with an AirPods headset in
// call mode. Not every overload broke it. Nothing says a tap is immune to
// whatever breaks there, so the tap watches itself and is rebuilt — a gap of
// a fraction of a second, not the rest of the take — when its IOProc stops
// being called, or when it delivers exact digital silence while another
// process is running audio output. Both tests wait for another process to
// be playing, because a tap's IO only starts with the first sound some
// process plays: before that, no buffers is the healthy state. The silence
// test also fires on an app that holds its output open and plays nothing,
// so rebuilds back off (30 s, doubling to 10 min) until the tap hears
// something again.
//
// The sys stream is lined up with the mic by sample index downstream
// (`build_playback_wav`, the timeline, the transcript order), and
// ScreenCaptureKit delivered buffers from the start, silence included. A
// tap delivers nothing until something plays and nothing across a rebuild,
// so `deliver` writes those spans as silence, timed from the host clock: the
// first buffer is padded back to when `start()` ran, and any later gap over
// 100 ms is filled up to the buffer that ends it.

enum TapError: Error, CustomStringConvertible {
    case status(String, OSStatus)
    case unavailable(String)

    var description: String {
        switch self {
        case .status(let what, let s): return "\(what) failed (OSStatus \(s))"
        case .unavailable(let what): return what
        }
    }
}

@available(macOS 14.2, *)
final class SystemAudioTap {
    private let queue = DispatchQueue(label: "tap.audio", qos: .userInteractive)
    private let control = DispatchQueue(label: "tap.control")
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private var format: AVAudioFormat?
    private var listening = false
    private var stopped = true
    private let output: SystemAudioOutput
    private var defaultOutputListener: AudioObjectPropertyListenerBlock?

    // Health, written on `queue` by `deliver` and read on `control`.
    private let health = NSLock()
    private var lastBuffer: Double = 0
    private var lastSignal: Double = 0
    // Health rebuilds, `control` only.
    private var rebuildBackoff: Double = SystemAudioTap.backoffFloor
    private var nextHealthRebuild: Double = 0
    private var failedRebuilds = 0
    // Alignment. `captureStart` is set before IO starts; `nextFrameAt` is
    // touched by `deliver` alone, on `queue`, and survives a rebuild so the
    // gap across it is filled.
    private var captureStart: Double = 0
    private var nextFrameAt: Double?

    static let stallSeconds: Double = 3
    static let silentSeconds: Double = 8
    static let backoffFloor: Double = 30
    static let backoffCeiling: Double = 600
    static let padThreshold: Double = 0.1
    static let padCeiling: Double = 6 * 3600

    init(output: SystemAudioOutput) {
        self.output = output
    }

    func start() throws {
        try control.sync {
            stopped = false
            captureStart = hostSecondsNow()
            try build()
            if !listening {
                addListeners()
                listening = true
            }
        }
    }

    func stop() {
        control.sync {
            stopped = true
            removeListeners()
            teardown()
        }
    }

    // MARK: build / teardown

    private func build() throws {
        let excluded = Self.processObjects(for: [getpid(), getppid()])
        let desc = CATapDescription(stereoGlobalTapButExcludeProcesses: excluded)
        desc.uuid = UUID()
        desc.name = "Humla system audio"
        desc.isPrivate = true
        desc.muteBehavior = .unmuted

        var tap = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(desc, &tap)
        guard err == noErr else { throw TapError.status("AudioHardwareCreateProcessTap", err) }
        tapID = tap

        var asbd = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        err = AudioObjectGetPropertyData(tapID, &addr, 0, nil, &size, &asbd)
        guard err == noErr else { teardown(); throw TapError.status("read tap format", err) }
        guard let fmt = AVAudioFormat(streamDescription: &asbd) else {
            teardown(); throw TapError.unavailable("tap format unsupported (sr=\(asbd.mSampleRate) ch=\(asbd.mChannelsPerFrame))")
        }
        format = fmt

        guard let outputUID = Self.defaultOutputUID() else {
            teardown(); throw TapError.unavailable("no default output device")
        }
        let aggregate: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Humla system audio",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: desc.uuid.uuidString,
            ]],
        ]
        var agg = AudioObjectID(kAudioObjectUnknown)
        err = AudioHardwareCreateAggregateDevice(aggregate as CFDictionary, &agg)
        guard err == noErr else { teardown(); throw TapError.status("AudioHardwareCreateAggregateDevice", err) }
        aggregateID = agg

        var proc: AudioDeviceIOProcID?
        err = AudioDeviceCreateIOProcIDWithBlock(&proc, aggregateID, queue) { [weak self] _, inInputData, inInputTime, _, _ in
            self?.deliver(inInputData, inInputTime)
        }
        guard err == noErr, let p = proc else { teardown(); throw TapError.status("AudioDeviceCreateIOProcIDWithBlock", err) }
        procID = p

        let now = hostSecondsNow()
        health.lock()
        lastBuffer = now
        lastSignal = now
        health.unlock()

        err = AudioDeviceStart(aggregateID, procID)
        guard err == noErr else { teardown(); throw TapError.status("AudioDeviceStart", err) }
        FileHandle.standardError.write(Data(
            "tap: capture started (sr=\(fmt.sampleRate)Hz channels=\(fmt.channelCount) interleaved=\(fmt.isInterleaved))\n".utf8
        ))
    }

    private func teardown() {
        if aggregateID != kAudioObjectUnknown {
            if let p = procID {
                AudioDeviceStop(aggregateID, p)
                AudioDeviceDestroyIOProcID(aggregateID, p)
            }
            AudioHardwareDestroyAggregateDevice(aggregateID)
        }
        if tapID != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tapID)
        }
        procID = nil
        aggregateID = AudioObjectID(kAudioObjectUnknown)
        tapID = AudioObjectID(kAudioObjectUnknown)
        format = nil
    }

    /// Rebuild after the default output device or the tap's format changed,
    /// or after `checkHealth` found the tap stalled or silent. The new
    /// interval is opened before the first buffer of the rebuilt tap, so
    /// `capture_timing` shows where the switch fell.
    private func rebuild(_ why: String) {
        control.async { [weak self] in
            guard let self = self, !self.stopped else { return }
            FileHandle.standardError.write(Data("tap: rebuilding (\(why))\n".utf8))
            self.teardown()
            sysTiming.openInterval("device_change")
            do {
                try self.build()
                self.failedRebuilds = 0
            } catch {
                // Retried from the 2 s poll; tell the app once, not every try.
                self.failedRebuilds += 1
                if self.failedRebuilds == 1 {
                    emitError("system audio: rebuilding the tap failed: \(error)")
                } else {
                    FileHandle.standardError.write(Data("tap: rebuild failed again (\(self.failedRebuilds)): \(error)\n".utf8))
                }
            }
        }
    }

    // MARK: IO

    private func deliver(_ abl: UnsafePointer<AudioBufferList>, _ time: UnsafePointer<AudioTimeStamp>) {
        guard let fmt = format else { return }
        let arrival = hostSecondsNow()
        let src = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: abl))
        guard src.count > 0, src[0].mDataByteSize > 0 else { return }
        let bytesPerFrame = max(1, Int(fmt.streamDescription.pointee.mBytesPerFrame))
        let frames = AVAudioFrameCount(Int(src[0].mDataByteSize) / bytesPerFrame)
        guard frames > 0, let copy = AVAudioPCMBuffer(pcmFormat: fmt, frameCapacity: frames) else { return }
        copy.frameLength = frames
        let dst = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
        for i in 0..<min(dst.count, src.count) {
            if let d = dst[i].mData, let s = src[i].mData {
                memcpy(d, s, Int(min(dst[i].mDataByteSize, src[i].mDataByteSize)))
            }
        }
        var stamp: Double? = nil
        if time.pointee.mFlags.contains(.hostTimeValid) {
            stamp = AVAudioTime.seconds(forHostTime: time.pointee.mHostTime)
        }
        let signal = Self.carriesSignal(src)
        health.lock()
        lastBuffer = arrival
        if signal { lastSignal = arrival }
        health.unlock()

        let at = stamp ?? arrival
        let gap = at - (nextFrameAt ?? captureStart)
        if gap > Self.padThreshold, gap < Self.padCeiling {
            padSilence(gap, format: fmt, from: at - gap, arrival: arrival)
        }
        nextFrameAt = at + Double(frames) / fmt.sampleRate
        output.ingest(copy, stamp: stamp, arrival: arrival)
    }

    /// Write `seconds` of silence in the tap's format, in one-second pieces,
    /// through the same `ingest` a real buffer takes.
    private func padSilence(_ seconds: Double, format fmt: AVAudioFormat, from start: Double, arrival: Double) {
        FileHandle.standardError.write(Data("tap: filling \(String(format: "%.2f", seconds)) s with silence\n".utf8))
        var remaining = AVAudioFrameCount((seconds * fmt.sampleRate).rounded())
        let piece = AVAudioFrameCount(fmt.sampleRate)
        var t = start
        while remaining > 0 {
            let n = min(piece, remaining)
            guard let buf = AVAudioPCMBuffer(pcmFormat: fmt, frameCapacity: n) else { return }
            buf.frameLength = n
            for b in UnsafeMutableAudioBufferListPointer(buf.mutableAudioBufferList) {
                if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) }
            }
            output.ingest(buf, stamp: t, arrival: arrival)
            t += Double(n) / fmt.sampleRate
            remaining -= n
        }
    }

    /// Any sample that is not an exact zero. A healthy tap of a call is
    /// almost never exactly zero for long — the far side's comfort noise and
    /// codec floor keep it off zero — which is what the ScreenCaptureKit
    /// failure lost.
    private static func carriesSignal(_ abl: UnsafeMutableAudioBufferListPointer) -> Bool {
        for buf in abl {
            guard let p = buf.mData else { continue }
            let words = Int(buf.mDataByteSize) / 4
            let w = p.bindMemory(to: UInt32.self, capacity: words)
            for i in 0..<words where w[i] & 0x7fff_ffff != 0 { return true }
        }
        return false
    }

    /// Run on `control` every 2 s. See the header for why.
    private func checkHealth() {
        guard !stopped, aggregateID != kAudioObjectUnknown else { return }
        let now = hostSecondsNow()
        health.lock()
        let sinceBuffer = now - lastBuffer
        let sinceSignal = now - lastSignal
        health.unlock()
        let stalled = sinceBuffer > Self.stallSeconds
        if !stalled, sinceSignal < Self.silentSeconds {
            rebuildBackoff = Self.backoffFloor
            nextHealthRebuild = 0
            return
        }
        guard now >= nextHealthRebuild, Self.anotherProcessIsPlaying() else { return }
        nextHealthRebuild = now + rebuildBackoff
        rebuildBackoff = min(rebuildBackoff * 2, Self.backoffCeiling)
        let why = stalled ? "no buffers for \(Int(sinceBuffer)) s" : "exact silence for \(Int(sinceSignal)) s"
        rebuild("\(why) while another process plays audio")
    }

    /// Whether any process other than Humla's own is running audio output.
    private static func anotherProcessIsPlaying() -> Bool {
        let own: Set<pid_t> = [getpid(), getppid()]
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let system = AudioObjectID(kAudioObjectSystemObject)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(system, &addr, 0, nil, &size) == noErr, size > 0 else { return false }
        var objects = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(system, &addr, 0, nil, &size, &objects) == noErr else { return false }
        for obj in objects {
            var pid: pid_t = 0
            var pidSize = UInt32(MemoryLayout<pid_t>.size)
            addr.mSelector = kAudioProcessPropertyPID
            guard AudioObjectGetPropertyData(obj, &addr, 0, nil, &pidSize, &pid) == noErr, !own.contains(pid) else { continue }
            var running: UInt32 = 0
            var runSize = UInt32(MemoryLayout<UInt32>.size)
            addr.mSelector = kAudioProcessPropertyIsRunningOutput
            if AudioObjectGetPropertyData(obj, &addr, 0, nil, &runSize, &running) == noErr, running != 0 { return true }
        }
        return false
    }

    // MARK: listeners

    private func addListeners() {
        var defaultOut = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let listener: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.rebuild("default output device changed")
        }
        AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultOut, control, listener)
        defaultOutputListener = listener
        // The tap's own format moves when the device it follows changes rate
        // without the default changing (AirPods entering call mode).
        let poll = DispatchSource.makeTimerSource(queue: control)
        poll.schedule(deadline: .now() + 2, repeating: 2)
        poll.setEventHandler { [weak self] in
            guard let self = self, !self.stopped else { return }
            guard self.tapID != kAudioObjectUnknown, let fmt = self.format else {
                // The last rebuild failed; keep trying rather than stay deaf.
                self.rebuild("retrying a failed rebuild")
                return
            }
            var asbd = AudioStreamBasicDescription()
            var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioTapPropertyFormat,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            if AudioObjectGetPropertyData(self.tapID, &addr, 0, nil, &size, &asbd) == noErr,
               asbd.mSampleRate != fmt.sampleRate || asbd.mChannelsPerFrame != fmt.channelCount {
                self.rebuild("tap format changed to \(asbd.mSampleRate)Hz")
                return
            }
            self.checkHealth()
        }
        poll.resume()
        formatPoll = poll
    }

    /// A paused capture builds a new tap on resume, so this one's listener
    /// and timer must go with it.
    private func removeListeners() {
        formatPoll?.cancel()
        formatPoll = nil
        if let listener = defaultOutputListener {
            var defaultOut = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultOut, control, listener)
            defaultOutputListener = nil
        }
        listening = false
    }

    private var formatPoll: DispatchSourceTimer?

    // MARK: helpers

    private static func processObjects(for pids: [pid_t]) -> [AudioObjectID] {
        var out: [AudioObjectID] = []
        for var pid in pids where pid > 1 {
            var obj = AudioObjectID(kAudioObjectUnknown)
            var size = UInt32(MemoryLayout<AudioObjectID>.size)
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            let err = AudioObjectGetPropertyData(
                AudioObjectID(kAudioObjectSystemObject), &addr,
                UInt32(MemoryLayout<pid_t>.size), &pid, &size, &obj
            )
            if err == noErr, obj != kAudioObjectUnknown { out.append(obj) }
        }
        return out
    }

    private static func defaultOutputUID() -> String? {
        var dev = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &dev) == noErr,
              dev != kAudioObjectUnknown else { return nil }
        var uid: Unmanaged<CFString>?
        size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        addr.mSelector = kAudioDevicePropertyDeviceUID
        guard AudioObjectGetPropertyData(dev, &addr, 0, nil, &size, &uid) == noErr, let u = uid else { return nil }
        return u.takeRetainedValue() as String
    }
}
