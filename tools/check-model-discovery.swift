import Foundation

/// Development probe: prints only public model IDs, never credentials.
@main
struct ModelDiscoveryProbe {
    static func main() async {
        let args = CommandLine.arguments
        let env = args.count > 1 ? SettingsEnvironment.read(URL(fileURLWithPath: args[1])) : [:]
        for provider in ["qwen", "doubao"] {
            do {
                let host = QwenConnection.host(custom: env["QWEN_API_HOST"] ?? "",
                    region: env["QWEN_REGION"] ?? "beijing", workspace: env["QWEN_WORKSPACE_ID"] ?? "")
                let result = try await SpeechModelDiscovery.refresh(provider: provider,
                    host: host, key: env["QWEN_API_KEY"] ?? "")
                print(provider, result.source, result.models.map(\.id).joined(separator: ", "))
            } catch {
                print(provider, "official discovery unavailable")
            }
        }
    }
}
