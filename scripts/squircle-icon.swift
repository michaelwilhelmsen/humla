// Mask an arbitrary PNG into a macOS-style squircle icon.
//
// Usage: swift squircle-icon.swift <input.png> <output.png>
//
// 1. Detects the bounding box of non-transparent pixels and crops to it
//    (so any blank margin in the source doesn't waste icon area).
// 2. Renders into a 1024×1024 canvas with a transparent border around an
//    824×824 squircle (Apple's standard ~80% icon area). The transparent
//    padding is what gives macOS room to draw the implicit shadow.
// 3. Clips the cropped image to a rounded-rect at 22.37% radius — the
//    macOS Big Sur convention. (Not a true continuous-corner squircle;
//    that requires Bezier hand-tuning that's overkill at this fidelity.)

import AppKit
import Foundation

guard CommandLine.arguments.count >= 3 else {
    FileHandle.standardError.write("usage: squircle-icon <input.png> <output.png>\n".data(using: .utf8)!)
    exit(64)
}

let inputPath = CommandLine.arguments[1]
let outputPath = CommandLine.arguments[2]

let canvasSize: CGFloat = 1024
let iconArea: CGFloat = 824                 // ~80% of canvas
let padding: CGFloat = (canvasSize - iconArea) / 2
let cornerRadius: CGFloat = iconArea * 0.2237

guard let inputImage = NSImage(contentsOfFile: inputPath),
      let inputCG = inputImage.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    FileHandle.standardError.write("failed to load \(inputPath)\n".data(using: .utf8)!)
    exit(1)
}

// Bounding box of non-transparent pixels.
let w = inputCG.width
let h = inputCG.height
let bytesPerRow = w * 4
var pixels = [UInt8](repeating: 0, count: h * bytesPerRow)
guard let probe = CGContext(
    data: &pixels,
    width: w,
    height: h,
    bitsPerComponent: 8,
    bytesPerRow: bytesPerRow,
    space: CGColorSpaceCreateDeviceRGB(),
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
) else { exit(1) }
probe.draw(inputCG, in: CGRect(x: 0, y: 0, width: w, height: h))

var minX = w, minY = h, maxX = -1, maxY = -1
for y in 0..<h {
    for x in 0..<w {
        if pixels[y * bytesPerRow + x * 4 + 3] > 10 {
            if x < minX { minX = x }
            if y < minY { minY = y }
            if x > maxX { maxX = x }
            if y > maxY { maxY = y }
        }
    }
}

let cropCG: CGImage
if maxX > minX && maxY > minY {
    let r = CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)
    cropCG = inputCG.cropping(to: r) ?? inputCG
} else {
    // No transparent pixels detected — assume the source is the full subject.
    cropCG = inputCG
}

// Compose output.
let output = NSImage(size: NSSize(width: canvasSize, height: canvasSize))
output.lockFocus()

NSColor.clear.set()
NSRect(x: 0, y: 0, width: canvasSize, height: canvasSize).fill()

let clip = NSBezierPath(
    roundedRect: NSRect(x: padding, y: padding, width: iconArea, height: iconArea),
    xRadius: cornerRadius,
    yRadius: cornerRadius
)
clip.addClip()

NSImage(cgImage: cropCG, size: .zero).draw(
    in: NSRect(x: padding, y: padding, width: iconArea, height: iconArea),
    from: .zero,
    operation: .sourceOver,
    fraction: 1.0
)

output.unlockFocus()

// Save as PNG.
guard let tiff = output.tiffRepresentation,
      let bmp = NSBitmapImageRep(data: tiff),
      let pngData = bmp.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("failed to encode PNG\n".data(using: .utf8)!)
    exit(1)
}
do {
    try pngData.write(to: URL(fileURLWithPath: outputPath))
    print("wrote \(outputPath) (\(Int(canvasSize))x\(Int(canvasSize)))")
} catch {
    FileHandle.standardError.write("failed to write \(outputPath): \(error)\n".data(using: .utf8)!)
    exit(1)
}
