import AppKit

let size = NSSize(width: 1024, height: 1024)
let image = NSImage(size: size)
image.lockFocus()

let canvas = NSRect(origin: .zero, size: size)
NSColor.clear.setFill()
canvas.fill()

// Match the visual footprint of neighbouring macOS Dock icons. The source
// artwork has its own shell; leave real transparency outside it, not a second
// full-canvas plate that makes the Dock icon appear oversized.
NSGraphicsContext.saveGraphicsState()
let artworkScale: CGFloat = 0.86
let artworkTransform = NSAffineTransform()
artworkTransform.translateX(by: 1024 * (1 - artworkScale) / 2,
                            yBy: 1024 * (1 - artworkScale) / 2)
artworkTransform.scale(by: artworkScale)
artworkTransform.concat()

let shellRect = canvas.insetBy(dx: 22, dy: 22)
let shell = NSBezierPath(roundedRect: shellRect, xRadius: 225, yRadius: 225)
NSColor(calibratedWhite: 0.97, alpha: 1).setFill()
shell.fill()

let tileRect = canvas.insetBy(dx: 58, dy: 58)
let tile = NSBezierPath(roundedRect: tileRect, xRadius: 206, yRadius: 206)
let tileGradient = NSGradient(colorsAndLocations:
    (NSColor(calibratedRed: 0.13, green: 0.08, blue: 0.34, alpha: 1), 0.0),
    (NSColor(calibratedRed: 0.23, green: 0.25, blue: 0.72, alpha: 1), 0.48),
    (NSColor(calibratedRed: 0.05, green: 0.69, blue: 0.78, alpha: 1), 1.0)
)!
tileGradient.draw(in: tile, angle: -34)

let glow = NSBezierPath(ovalIn: NSRect(x: 170, y: 210, width: 690, height: 690))
NSColor.white.withAlphaComponent(0.075).setFill()
glow.fill()

NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.24)
shadow.shadowBlurRadius = 28
shadow.shadowOffset = NSSize(width: 0, height: -12)
shadow.set()

let heights: [CGFloat] = [150, 250, 360, 455, 330, 215, 120]
let widths: [CGFloat] = [42, 46, 50, 54, 50, 46, 42]
let gap: CGFloat = 31
let totalWidth = widths.reduce(0, +) + gap * CGFloat(widths.count - 1)
var x = (1024 - totalWidth) / 2 - 22
let centerY: CGFloat = 515

for index in heights.indices {
    let height = heights[index]
    let width = widths[index]
    let barRect = NSRect(x: x, y: centerY - height / 2, width: width, height: height)
    let bar = NSBezierPath(roundedRect: barRect, xRadius: width / 2, yRadius: width / 2)
    NSColor.white.withAlphaComponent(index == 3 ? 0.98 : 0.88).setFill()
    bar.fill()
    x += width + gap
}

let caretRect = NSRect(x: 768, y: 315, width: 38, height: 400)
let caret = NSBezierPath(roundedRect: caretRect, xRadius: 19, yRadius: 19)
NSColor(calibratedRed: 0.55, green: 0.98, blue: 1.0, alpha: 0.98).setFill()
caret.fill()
NSGraphicsContext.restoreGraphicsState()
NSGraphicsContext.restoreGraphicsState()

image.unlockFocus()
guard let data = image.tiffRepresentation,
      let bitmap = NSBitmapImageRep(data: data),
      let png = bitmap.representation(using: .png, properties: [:]) else {
    fatalError("Unable to render app icon")
}
let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "AppIcon-1024.png"
try png.write(to: URL(fileURLWithPath: output))
