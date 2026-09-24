/// Converts community-1's stored clustering threshold to the value FluidAudio takes.
///
/// Humla stores it as a similarity cutoff, where a higher value means more speakers.
/// FluidAudio reads it as a distance between unit-length embeddings, where a higher
/// value means fewer, and accepts (0, 2]. The two are related by d = √(2 − 2s).
public func fluidAudioClusteringThreshold(fromStored stored: Double) -> Double {
    guard !stored.isNaN else { return fluidAudioClusteringThreshold(fromStored: 0.5) }
    let similarity = min(max(stored, -1), 1)
    return max((2 - 2 * similarity).squareRoot(), 0.01)
}
