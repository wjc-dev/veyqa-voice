import Foundation

struct SpeechModel: Codable, Identifiable, Hashable {
    var provider: String
    var id: String
    var name: String
}

struct SpeechModelSeed: Decodable {
    var defaults: [String: String]
    var models: [SpeechModel]
}

struct SpeechModelSnapshot: Codable {
    var models: [SpeechModel]
    var checkedAt: Date
    var source: String
}

/// Model versions are data. Only the wire-protocol families live in code.
enum SpeechModels {
    static let qwenPattern = #"qwen-audio-\d+(?:\.\d+)*-asr-flash-(?:streaming|message)(?:-\d{4}-\d{2}-\d{2})?"#
    static let doubaoPattern = #"volc\.[a-z0-9]+\.sauc\.(?:duration|concurrent)"#
    static let seed: SpeechModelSeed = {
        let source = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("config/asr-models.json")
        let url = Bundle.main.url(forResource: "asr-models", withExtension: "json") ?? source
        // A missing catalogue is caught during packaging, before installation.
        return try! JSONDecoder().decode(SpeechModelSeed.self, from: Data(contentsOf: url))
    }()

    static func defaultModel(_ provider: String) -> String { seed.defaults[provider]! }

    static func configuredModel(_ value: String?, provider: String) -> String {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? defaultModel(provider) : trimmed
    }

    static func compatible(_ id: String, provider: String) -> Bool {
        let pattern = provider == "qwen" ? qwenPattern : doubaoPattern
        return id.range(of: "^(?:" + pattern + ")$", options: .regularExpression) != nil
    }

    static func option(_ id: String, provider: String) -> SpeechModel {
        seed.models.first { $0.id == id && $0.provider == provider }
            ?? SpeechModel(provider: provider, id: id, name: id)
    }

    static func merge(_ models: [SpeechModel], provider: String) -> [SpeechModel] {
        var seen = Set<String>()
        return models.filter {
            $0.provider == provider && compatible($0.id, provider: provider)
                && seen.insert($0.id).inserted
        }.sorted { $0.id.compare($1.id, options: .numeric) == .orderedDescending }
    }

    static func fromOfficialText(_ text: String, provider: String) -> [SpeechModel] {
        let family = provider == "qwen" ? qwenPattern : doubaoPattern
        let regex = try! NSRegularExpression(pattern: "(?<![A-Za-z0-9_-])(?:" + family + ")(?![A-Za-z0-9_-])")
        let ids = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))
            .compactMap { Range($0.range, in: text).map { String(text[$0]) } }
        return merge(ids.map { option($0, provider: provider) }, provider: provider)
    }

    static func fromAPI(_ json: [String: Any]) -> (models: [SpeechModel], total: Int)? {
        guard let output = json["output"] as? [String: Any],
              let records = output["models"] as? [[String: Any]],
              let total = output["total"] as? Int else { return nil }
        let models = records.compactMap { record -> SpeechModel? in
            guard let id = record["model"] as? String, compatible(id, provider: "qwen") else { return nil }
            return option(id, provider: "qwen")
        }
        return (merge(models, provider: "qwen"), total)
    }
}

private final class ModelHTTPDelegate: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        // Never forward a model-list Authorization header through a redirect.
        completionHandler(nil)
    }
}

enum ModelDiscoveryError: Error { case unavailable }

enum SpeechModelDiscovery {
    static func refresh(provider: String, host: String?, key: String) async throws -> SpeechModelSnapshot {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 8
        config.timeoutIntervalForResource = 15
        let session = URLSession(configuration: config, delegate: ModelHTTPDelegate(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        if provider == "qwen", let host, !key.isEmpty {
            do {
                var all: [SpeechModel] = []
                for page in 1...10 {
                    let url = URL(string: "https://\(host)/api/v1/models?capabilities=ASR&page_no=\(page)&page_size=100")!
                    var request = URLRequest(url: url)
                    request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
                    let data = try await fetch(request, session: session)
                    guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                          let result = SpeechModels.fromAPI(json) else { throw ModelDiscoveryError.unavailable }
                    all += result.models
                    if page * 100 >= result.total {
                        guard !all.isEmpty else { throw ModelDiscoveryError.unavailable }
                        return SpeechModelSnapshot(models: SpeechModels.merge(all, provider: provider),
                                                   checkedAt: Date(), source: "百炼官方 API")
                    }
                }
            } catch is CancellationError { throw CancellationError() }
            catch { /* Some account/region endpoints do not expose Models.list. */ }
        }
        try Task.checkCancellation()
        let url = URL(string: provider == "qwen"
            ? "https://help.aliyun.com/zh/model-studio/model-list-speech-recognition/"
            : "https://docs.volcengine.com/api/doc/getDocDetail?LibraryCode=DoubaoVoice&DocumentCode=LargemodelstreamingautomaticspeechrecognitionAPI")!
        let data = try await fetch(URLRequest(url: url), session: session)
        let models = SpeechModels.fromOfficialText(String(decoding: data, as: UTF8.self), provider: provider)
        guard !models.isEmpty else { throw ModelDiscoveryError.unavailable }
        return SpeechModelSnapshot(models: models, checkedAt: Date(),
                                   source: provider == "qwen" ? "百炼官方目录" : "火山引擎官方目录")
    }

    private static func fetch(_ request: URLRequest, session: URLSession) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200,
              data.count <= 4_000_000 else { throw ModelDiscoveryError.unavailable }
        return data
    }
}
