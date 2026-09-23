import Foundation

// Per-stream delivery timing, for the `capture_timing` diagnostic.
//
// Each stream's full WAV — and every chunk's `start_ms` — counts frames from
// that stream's own first delivered frame, and `build_playback_wav` sums the
// two streams sample by sample from index 0. Nothing measured whether index 0
// of mic-full.wav and index 0 of sys-full.wav are the same instant, or whether
// a stream keeps pace with the clock across silence, pauses and device
// changes. This does, so a start offset, delivery gaps and clock drift can be
// read apart from the acoustic and output latency an echo measurement also
// contains.
//
// Foundation only, on purpose: the caller converts its buffers' timestamps to
// host seconds, and this does arithmetic on them. That keeps it compilable on
// its own, away from AVFoundation and ScreenCaptureKit. Timestamps and counts
// only — no audio, and no device names (#174).

/// A buffer whose timestamp lands more than this far from where the previous
/// one ended is counted as a gap (later) or an overlap (earlier) rather than as
/// jitter. `max_jitter_ms` reports what stayed under it, so the threshold can
/// be checked against the stream it was applied to.
let deliveryGapThreshold: Double = 0.005

/// Each interval keeps the position of at most this many discontinuities; the
/// counts and totals stay exact beyond it.
let maxJumpsPerInterval = 64

/// One stream's delivery, interval by interval. An interval opens at the first
/// buffer after start, resume or (mic only) a device change; every buffer after
/// that is checked against where the previous one ended.
final class StreamTiming {
    private struct Interval {
        /// What opened it: "start", "resume" or "device_change".
        let openedBy: String
        /// 16 kHz frames this stream had written before the interval — where
        /// its first frame landed in the full WAV.
        let startFrame: Int
        /// Host time of the interval's first frame: the buffer's own timestamp,
        /// or its arrival when it carried none (`stampFromArrival`).
        let firstStamp: Double
        let firstArrival: Double
        let stampFromArrival: Bool
        /// Host time just past the last frame noted so far.
        var lastEndStamp: Double
        var lastArrival: Double
        /// Seconds of audio delivered, each buffer at its own sample rate.
        var mediaSeconds: Double
        var inRate: Double
        var outFrames: Int
        var buffers: Int = 1
        var gaps: Int = 0
        var gapSeconds: Double = 0
        var maxGapSeconds: Double = 0
        var overlaps: Int = 0
        var overlapSeconds: Double = 0
        var maxJitterSeconds: Double = 0
        /// Where each gap or overlap fell: the WAV frame the buffer after it
        /// landed at, and its size (positive a gap, negative an overlap). What
        /// lets a step in a measured echo lag be matched to the frames that
        /// caused it.
        var jumps: [(frame: Int, seconds: Double)] = []
    }

    private let lock = NSLock()
    private var intervals: [Interval] = []
    private var pendingOpen: String? = "start"
    private var framesWritten = 0

    /// The next buffer opens a new interval instead of continuing the current
    /// one, so a pause or a re-tap reads as a boundary rather than as a gap.
    func openInterval(_ reason: String) {
        lock.lock()
        pendingOpen = reason
        lock.unlock()
    }

    /// Record one buffer that reached the writers.
    ///
    /// - `stamp`: host seconds of its first frame, `nil` if it carried none.
    /// - `arrival`: host seconds when the callback received it.
    /// - `inFrames` / `inRate`: its length at the source rate.
    /// - `outFrames`: the 16 kHz frames it put into the full WAV — 0 when the
    ///   converter held them back or the write was skipped.
    func note(stamp: Double?, arrival: Double, inFrames: Int, inRate: Double, outFrames: Int) {
        let duration = inRate > 0 ? Double(inFrames) / inRate : 0
        let at = stamp ?? arrival
        lock.lock()
        defer { lock.unlock() }
        let startFrame = framesWritten
        framesWritten += outFrames
        if let reason = pendingOpen {
            pendingOpen = nil
            intervals.append(Interval(
                openedBy: reason,
                startFrame: startFrame,
                firstStamp: at,
                firstArrival: arrival,
                stampFromArrival: stamp == nil,
                lastEndStamp: at + duration,
                lastArrival: arrival,
                mediaSeconds: duration,
                inRate: inRate,
                outFrames: outFrames
            ))
            return
        }
        guard var current = intervals.last else { return }
        let jump = at - current.lastEndStamp
        if jump > deliveryGapThreshold {
            current.gaps += 1
            current.gapSeconds += jump
            current.maxGapSeconds = max(current.maxGapSeconds, jump)
        } else if jump < -deliveryGapThreshold {
            current.overlaps += 1
            current.overlapSeconds -= jump
        } else {
            current.maxJitterSeconds = max(current.maxJitterSeconds, abs(jump))
        }
        if abs(jump) > deliveryGapThreshold && current.jumps.count < maxJumpsPerInterval {
            current.jumps.append((frame: startFrame, seconds: jump))
        }
        current.lastEndStamp = at + duration
        current.lastArrival = arrival
        current.mediaSeconds += duration
        current.inRate = inRate
        current.outFrames += outFrames
        current.buffers += 1
        intervals[intervals.count - 1] = current
    }

    /// This stream's timing, ready for JSONSerialization: every host time made
    /// relative to `epoch`, every duration in milliseconds.
    func report(source: String, epoch: Double) -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return [
            "source": source,
            "frames_written": framesWritten,
            "intervals": intervals.map { i -> [String: Any] in
                [
                    "opened_by": i.openedBy,
                    "start_frame": i.startFrame,
                    "first_stamp_ms": millis(i.firstStamp - epoch),
                    "first_arrival_ms": millis(i.firstArrival - epoch),
                    "stamp_from_arrival": i.stampFromArrival,
                    "last_end_stamp_ms": millis(i.lastEndStamp - epoch),
                    "last_arrival_ms": millis(i.lastArrival - epoch),
                    "media_ms": millis(i.mediaSeconds),
                    "in_rate": jsonSafe(i.inRate),
                    "out_frames": i.outFrames,
                    "buffers": i.buffers,
                    "gaps": i.gaps,
                    "gap_ms": millis(i.gapSeconds),
                    "max_gap_ms": millis(i.maxGapSeconds),
                    "overlaps": i.overlaps,
                    "overlap_ms": millis(i.overlapSeconds),
                    "max_jitter_ms": millis(i.maxJitterSeconds),
                    "jumps": i.jumps.map { j -> [Any] in [j.frame, millis(j.seconds)] },
                ]
            },
        ]
    }
}

/// JSONSerialization raises an Objective-C exception — not a Swift error, so
/// nothing can catch it — on NaN or infinity. Anything non-finite is written
/// as 0 rather than trusted.
func jsonSafe(_ value: Double) -> Double {
    value.isFinite ? value : 0
}

/// Seconds to milliseconds, rounded to the microsecond.
func millis(_ seconds: Double) -> Double {
    (jsonSafe(seconds) * 1_000_000).rounded() / 1000
}

/// A CoreAudio four-char code as its four characters (`bltn`, `blue`,
/// `usb `…), or as a decimal number when it isn't printable ASCII.
func fourCCString(_ code: UInt32) -> String {
    let bytes: [UInt8] = [24, 16, 8, 0].map { UInt8((code >> UInt32($0)) & 0xff) }
    if bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7f }) {
        return String(decoding: bytes, as: UTF8.self)
    }
    return String(code)
}
