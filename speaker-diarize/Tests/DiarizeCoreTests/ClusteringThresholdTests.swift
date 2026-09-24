import Testing
@testable import DiarizeCore

@Test func humlaDefaultMapsToADistanceOfOne() {
    #expect(abs(fluidAudioClusteringThreshold(fromStored: 0.5) - 1.0) < 1e-12)
}

@Test func mapsBySquareRootOfTwoMinusTwiceTheStoredValue() {
    #expect(abs(fluidAudioClusteringThreshold(fromStored: 0.6) - 0.8.squareRoot()) < 1e-12)
    #expect(abs(fluidAudioClusteringThreshold(fromStored: 0.0) - 2.0.squareRoot()) < 1e-12)
}

@Test func aHigherStoredValueStillMeansMoreSpeakers() {
    // A smaller distance cut merges less, which is what a higher stored value
    // always meant.
    let stored = [0.1, 0.3, 0.5, 0.7, 0.9]
    let distances = stored.map { fluidAudioClusteringThreshold(fromStored: $0) }
    #expect(distances == distances.sorted(by: >))
}

@Test func staysInsideTheRangeFluidAudioAccepts() {
    for stored in [-5.0, -1.0, 0.99, 1.0, 1.5, .nan, .infinity] {
        let distance = fluidAudioClusteringThreshold(fromStored: stored)
        #expect(distance > 0 && distance <= 2, "stored \(stored) mapped to \(distance)")
    }
}
