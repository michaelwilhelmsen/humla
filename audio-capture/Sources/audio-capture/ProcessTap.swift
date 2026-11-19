import AVFoundation
import CoreAudio
import Foundation

// System audio from a Core Audio process tap (macOS 14.2+), fed through
// `SystemAudioOutput.ingest` like a ScreenCaptureKit buffer. A tap's format
// follows the output device, so it is rebuilt when the default output or its
// format changes, and when it stalls or delivers exact silence while another
// process plays. It delivers nothing until something plays and nothing across a
// rebuild, while sys is lined up with the mic by sample index downstream, so
// `deliver` writes those spans as silence timed from the host clock.

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
    private var tapDescription: CATapDescription?
    private var excluded: [AudioObjectID] = []
    private var listening = false
    private var stopped = true
    private let output: SystemAudioOutput
    private var defaultOutputListener: AudioObjectPropertyListenerBlock?
    private var processListListener: AudioObjectPropertyListenerBlock?
    // Set once TCC reports the permission granted, `control` only.
    private var permissionConfirmed = false

    // Health, written on `queue` by `deliver` and read on `control`.
    private let health = NSLock()
    private var lastBuffer: Double = 0
    private var lastSignal: Double = 0
    private var everSignal = false
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
        control.sync { shutDown() }
        // A buffer from this tap still queued must not reach the writers after
        // a resume's new tap has started.
        queue.sync {}
    }

    /// `control` only.
    private func shutDown() {
        stopped = true
        removeListeners()
        teardown()
    }

    // MARK: build / teardown

    private func build() throws {
        let own = Self.ownProcessObjects()
        let desc = CATapDescription(stereoGlobalTapButExcludeProcesses: own)
        desc.uuid = UUID()
        desc.name = "Humla system audio"
        desc.isPrivate = true
        desc.muteBehavior = .unmuted

        var tap = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(desc, &tap)
        guard err == noErr else { throw TapError.status("AudioHardwareCreateProcessTap", err) }
        tapID = tap
        tapDescription = desc
        excluded = own

        guard var asbd = Self.tapFormat(tapID) else { teardown(); throw TapError.unavailable("read tap format failed") }
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
            self?.deliver(inInputData, inInputTime, format: fmt)
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
        tapDescription = nil
        excluded = []
    }

    /// The new `capture_timing` interval opens before the rebuilt tap's first
    /// buffer, so it shows where the rebuild fell.
    private func rebuild(_ why: String, opening interval: String = "tap_rebuild") {
        control.async { [weak self] in
            guard let self = self, !self.stopped else { return }
            FileHandle.standardError.write(Data("tap: rebuilding (\(why))\n".utf8))
            self.teardown()
            sysTiming.openInterval(interval)
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

    /// Runs on `queue`. Takes the format its IOProc was built with rather than
    /// reading `format`, which `control` rewrites during a rebuild.
    private func deliver(_ abl: UnsafePointer<AudioBufferList>, _ time: UnsafePointer<AudioTimeStamp>, format fmt: AVAudioFormat) {
        let arrival = hostSecondsNow()
        let all = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: abl))
        // The aggregate lists its sub-device's input streams before the tap's.
        let tapBuffers = fmt.isInterleaved ? 1 : Int(fmt.channelCount)
        guard all.count >= tapBuffers else { return }
        let src = Array(all.suffix(tapBuffers))
        guard src[0].mDataByteSize > 0 else { return }
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
        if signal {
            lastSignal = arrival
            everSignal = true
        }
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

    /// Any sample that is not an exact zero. A call's comfort noise and codec
    /// floor keep a healthy tap off zero.
    private static func carriesSignal(_ abl: [AudioBuffer]) -> Bool {
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

    /// A tap the user turned down at macOS's prompt records silence, so it is
    /// stopped and the app told. Only before the tap has carried any sound,
    /// which a refused tap never does.
    private func checkPermission() {
        guard !permissionConfirmed, !stopped else { return }
        switch SystemAudioPermission.status() {
        case "granted":
            permissionConfirmed = true
        case "denied":
            health.lock()
            let heard = everSignal
            health.unlock()
            if heard {
                permissionConfirmed = true
                return
            }
            emitError("System audio not allowed — only your microphone is being recorded. Allow it in Settings → Permissions.")
            shutDown()
        default:
            break
        }
    }

    /// Whether any process other than Humla's own is running audio output.
    private static func anotherProcessIsPlaying() -> Bool {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyIsRunningOutput,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        for (obj, pid) in processes() where !isOwn(pid) {
            var running: UInt32 = 0
            var size = UInt32(MemoryLayout<UInt32>.size)
            if AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &running) == noErr, running != 0 { return true }
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
            self?.rebuild("default output device changed", opening: "device_change")
        }
        AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultOut, control, listener)
        defaultOutputListener = listener
        var processList = Self.processListAddress
        let processListener: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            guard let self = self, !self.stopped else { return }
            self.excludeNewOwnProcesses()
        }
        AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &processList, control, processListener)
        processListListener = processListener
        // The tap's own format moves when the device it follows changes rate
        // without the default changing (AirPods entering call mode).
        let poll = DispatchSource.makeTimerSource(queue: control)
        poll.schedule(deadline: .now() + 2, repeating: 2)
        poll.setEventHandler { [weak self] in
            guard let self = self, !self.stopped else { return }
            guard self.tapID != kAudioObjectUnknown, let fmt = self.format else {
                // The last rebuild failed; keep trying rather than stay deaf.
                self.rebuild("retrying a failed rebuild", opening: "device_change")
                return
            }
            if let asbd = Self.tapFormat(self.tapID),
               asbd.mSampleRate != fmt.sampleRate || asbd.mChannelsPerFrame != fmt.channelCount {
                self.rebuild("tap format changed to \(asbd.mSampleRate)Hz", opening: "device_change")
                return
            }
            self.checkPermission()
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
        if let listener = processListListener {
            var processList = Self.processListAddress
            AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &processList, control, listener)
            processListListener = nil
        }
        listening = false
    }

    private var formatPoll: DispatchSourceTimer?

    /// Humla's WebKit processes only appear in Core Audio once they play
    /// something, so they join the exclusion list when the process list
    /// changes. Rebuilds when the tap won't take the new list.
    private func excludeNewOwnProcesses() {
        guard let desc = tapDescription else { return }
        let own = Self.ownProcessObjects()
        guard own != excluded else { return }
        desc.processes = own
        var ref = Unmanaged.passUnretained(desc).toOpaque()
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyDescription,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let err = AudioObjectSetPropertyData(tapID, &addr, 0, nil, UInt32(MemoryLayout<UnsafeMutableRawPointer>.size), &ref)
        if err == noErr {
            excluded = own
            FileHandle.standardError.write(Data("tap: now leaving out \(own.count) of Humla's processes\n".utf8))
        } else {
            rebuild("Humla's processes changed (OSStatus \(err))")
            excluded = own
        }
    }

    // MARK: helpers

    private typealias ResponsiblePID = @convention(c) (pid_t) -> pid_t
    private static let responsiblePID: ResponsiblePID? = {
        guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_get_pid_responsible_for_pid") else { return nil }
        return unsafeBitCast(sym, to: ResponsiblePID.self)
    }()

    /// This sidecar, the app that launched it, and every process macOS holds
    /// the app responsible for (its WebKit content and GPU processes).
    private static func isOwn(_ pid: pid_t) -> Bool {
        let app = getppid()
        if pid == getpid() || pid == app { return true }
        guard app > 1, let responsible = responsiblePID else { return false }
        return responsible(pid) == app
    }

    /// The Core Audio process objects of `isOwn` processes, sorted so two
    /// reads compare equal when nothing changed.
    private static func ownProcessObjects() -> [AudioObjectID] {
        processes().filter { isOwn($0.pid) }.map(\.object).sorted()
    }

    private static let processListAddress = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyProcessObjectList,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )

    /// Every process Core Audio knows, with its PID.
    private static func processes() -> [(object: AudioObjectID, pid: pid_t)] {
        let system = AudioObjectID(kAudioObjectSystemObject)
        var addr = processListAddress
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(system, &addr, 0, nil, &size) == noErr, size > 0 else { return [] }
        var objects = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(system, &addr, 0, nil, &size, &objects) == noErr else { return [] }
        addr.mSelector = kAudioProcessPropertyPID
        return objects.compactMap { obj in
            var pid: pid_t = 0
            var pidSize = UInt32(MemoryLayout<pid_t>.size)
            guard AudioObjectGetPropertyData(obj, &addr, 0, nil, &pidSize, &pid) == noErr else { return nil }
            return (obj, pid)
        }
    }

    private static func tapFormat(_ tap: AudioObjectID) -> AudioStreamBasicDescription? {
        var asbd = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        guard AudioObjectGetPropertyData(tap, &addr, 0, nil, &size, &asbd) == noErr else { return nil }
        return asbd
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
