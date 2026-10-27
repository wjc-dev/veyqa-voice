import SwiftUI

enum SettingsMotion {
    static let selection = Animation.spring(response: 0.28, dampingFraction: 0.86)
    static let page = Animation.easeOut(duration: 0.18)
    static let reveal = Animation.easeOut(duration: 0.16)
}

struct SettingsSegment: Identifiable {
    let id: String
    let title: String
    var symbol: String? = nil
}

/// One shared selection surface for tabs, service modes and providers.
/// The highlight moves; labels and hit targets stay in place.
struct SettingsSegmentedControl: View {
    let title: String
    @Binding var selection: String
    let options: [SettingsSegment]
    @Namespace private var highlight
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private func select(_ id: String) {
        guard selection != id else { return }
        selection = id
    }

    var body: some View {
        HStack(spacing: 3) {
            ForEach(options) { option in
                Button { select(option.id) } label: {
                    HStack(spacing: 6) {
                        if let symbol = option.symbol {
                            Image(systemName: symbol)
                                .foregroundStyle(selection == option.id ? Color.accentColor : Color.secondary)
                                .accessibilityHidden(true)
                        }
                        Text(option.title).fontWeight(selection == option.id ? .semibold : .medium)
                    }
                    .frame(maxWidth: .infinity, minHeight: 30)
                    .contentShape(Rectangle())
                }
                .buttonStyle(SettingsSegmentButtonStyle())
                .background {
                    if selection == option.id {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(.background)
                            .overlay {
                                RoundedRectangle(cornerRadius: 8, style: .continuous)
                                    .fill(LinearGradient(colors: [Color.accentColor.opacity(0.08), .clear],
                                                         startPoint: .topLeading, endPoint: .bottomTrailing))
                            }
                            .overlay {
                                RoundedRectangle(cornerRadius: 8, style: .continuous)
                                    .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5)
                            }
                            .shadow(color: .black.opacity(0.09), radius: 3, y: 1)
                            .matchedGeometryEffect(id: "selection", in: highlight)
                    }
                }
                .accessibilityLabel(option.title)
                .accessibilityAddTraits(selection == option.id ? .isSelected : [])
            }
        }
        .padding(4)
        .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .animation(reduceMotion ? nil : SettingsMotion.selection, value: selection)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
        .onMoveCommand { direction in
            guard let index = options.firstIndex(where: { $0.id == selection }) else { return }
            if direction == .right { select(options[min(index + 1, options.count - 1)].id) }
            if direction == .left { select(options[max(index - 1, 0)].id) }
        }
    }
}

private struct SettingsSegmentButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovered = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.primary)
            .background(Color.primary.opacity(hovered ? 0.035 : 0), in: RoundedRectangle(cornerRadius: 8))
            .opacity(isEnabled ? (configuration.isPressed ? 0.65 : 1) : 0.45)
            .scaleEffect(reduceMotion || !configuration.isPressed ? 1 : 0.985)
            .onHover { hovered = $0 }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.10), value: hovered)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.10), value: configuration.isPressed)
    }
}

/// Animate content appearance without moving the disclosure header or scrolling it away.
private struct SettingsReveal: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var visible = false

    func body(content: Content) -> some View {
        content
            .opacity(visible || reduceMotion ? 1 : 0)
            .offset(y: visible || reduceMotion ? 0 : 5)
            .onAppear { withAnimation(reduceMotion ? nil : SettingsMotion.reveal) { visible = true } }
    }
}

/// Feedback changes appearance, never geometry or the control's hit target.
struct SettingsRowButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isHovered = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
            .background(Color.primary.opacity(
                !isEnabled ? 0 : configuration.isPressed ? 0.10 : isHovered ? 0.045 : 0
            ), in: RoundedRectangle(cornerRadius: 6))
            .opacity(isEnabled ? 1 : 0.45)
            .onHover { isHovered = $0 }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.10), value: isHovered)
    }
}

struct SettingsIconButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isHovered = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(width: 28, height: 28)
            .contentShape(Rectangle())
            .background(Color.primary.opacity(!isEnabled ? 0 : configuration.isPressed ? 0.10 : isHovered ? 0.06 : 0),
                        in: RoundedRectangle(cornerRadius: 6))
            .opacity(isEnabled ? 1 : 0.4)
            .onHover { isHovered = $0 }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.10), value: isHovered)
    }
}

/// The header stays above its content. Content fades in without animating layout.
struct SettingsDisclosureStyle: DisclosureGroupStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func makeBody(configuration: Configuration) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { configuration.isExpanded.toggle() } label: {
                HStack(spacing: 8) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                        .rotationEffect(.degrees(configuration.isExpanded ? 90 : 0))
                        .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: configuration.isExpanded)
                        .frame(width: 12)
                        .accessibilityHidden(true)
                    configuration.label
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 30)
                .contentShape(Rectangle())
            }
            .buttonStyle(SettingsRowButtonStyle())
            .accessibilityValue(configuration.isExpanded ? "已展开" : "已收起")
            .onMoveCommand { direction in
                if direction == .right { configuration.isExpanded = true }
                if direction == .left { configuration.isExpanded = false }
            }
            if configuration.isExpanded {
                VStack(alignment: .leading, spacing: 12) { configuration.content }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 10)
                    .padding(.horizontal, 6)
                    .padding(.bottom, 6)
                    .transition(.identity)
                    .modifier(SettingsReveal())
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }
}

struct SettingsActionRow: View {
    let title: String
    var detail = ""
    var systemImage: String? = nil
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                if let systemImage { Image(systemName: systemImage).foregroundStyle(.secondary) }
                Text(title)
                Spacer()
                if !detail.isEmpty { Text(detail).foregroundStyle(.secondary) }
                Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
            }
            .frame(minHeight: 30)
        }
        .buttonStyle(SettingsRowButtonStyle())
    }
}

struct SettingsToggleRow: View {
    let title: String
    @Binding var isOn: Bool

    var body: some View {
        HStack(spacing: 12) {
            Button { isOn.toggle() } label: {
                HStack { Text(title); Spacer(minLength: 0) }
                    .frame(minHeight: 30).contentShape(Rectangle())
            }
            .buttonStyle(SettingsRowButtonStyle())
            .focusable(false)
            .accessibilityHidden(true)
            Toggle(title, isOn: $isOn).labelsHidden().accessibilityLabel(title)
        }
    }
}

struct CredentialHelpView: View {
    let provider: String
    @Environment(\.dismiss) private var dismiss

    private var steps: [String] {
        switch provider {
        case "doubao": return [
            "登录豆包语音控制台，在旧版控制台的「API 服务中心」选择或创建应用。",
            "开通流式语音识别服务，复制该应用的 App ID 和 Access Token。",
            "回到这里分别粘贴，选择已开通的模型后保存。"
        ]
        default: return [
            "登录阿里云百炼控制台，按页面提示开通语音服务。",
            "在 API Key 页面创建并复制 Key，默认地区为中国内地（北京）。",
            "回到这里粘贴并连接语音服务；其他地区可在高级设置中选择。"
        ]
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("获取 API 凭证").font(.headline)
                Spacer()
                Text(provider == "doubao" ? "豆包 · 火山引擎" : "千问 · 阿里云百炼")
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                HStack(alignment: .top, spacing: 10) {
                    Text("\(index + 1)").font(.caption.weight(.semibold))
                        .frame(width: 22, height: 22)
                        .background(Color.accentColor.opacity(0.10), in: Circle())
                    Text(step).fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack {
                Link("打开控制台 ↗", destination: URL(string: provider == "doubao"
                    ? "https://console.volcengine.com/speech/app"
                    : "https://bailian.console.aliyun.com/?tab=model#/api-key")!)
                Link("官方获取指南 ↗", destination: URL(string: provider == "doubao"
                    ? "https://docs.volcengine.com/docs/DoubaoVoice/console-guidance?lang=zh"
                    : "https://help.aliyun.com/zh/model-studio/get-api-key")!)
                Spacer()
                Button("完成") { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(width: 430)
    }
}
