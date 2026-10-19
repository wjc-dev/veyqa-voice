import AppKit

// Finder places the two draggable icons over this static, code-rendered guide.
let image = NSImage(size: NSSize(width: 720, height: 480))
image.lockFocus()
NSColor(calibratedRed: 0.96, green: 0.98, blue: 0.99, alpha: 1).setFill()
NSRect(x: 0, y: 0, width: 720, height: 480).fill()
func text(_ value: String, y: CGFloat, size: CGFloat, strong: Bool = false) {
    let paragraph = NSMutableParagraphStyle()
    paragraph.alignment = .center
    (value as NSString).draw(in: NSRect(x: 24, y: y, width: 672, height: 44), withAttributes: [
        .font: NSFont.systemFont(ofSize: size, weight: strong ? .semibold : .regular),
        .foregroundColor: NSColor(calibratedRed: 0.13, green: 0.20, blue: 0.27, alpha: 1),
        .paragraphStyle: paragraph,
    ])
}
text("安装 Veyqa Voice", y: 408, size: 28, strong: true)
text("将左侧 App 拖入右侧 Applications（应用程序）", y: 370, size: 16)
text("→", y: 270, size: 44)
text("复制完成后，从“应用程序”打开 Veyqa Voice", y: 185, size: 16)
text("首次打开与权限设置，请查看下方说明", y: 145, size: 13)
text("轻量语音输入 · 与你的输入法一起使用", y: 8, size: 12)
image.unlockFocus()
guard let data = image.tiffRepresentation,
      let bitmap = NSBitmapImageRep(data: data),
      let png = bitmap.representation(using: .png, properties: [:]) else {
    fatalError("Unable to render installation guide")
}
try png.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
