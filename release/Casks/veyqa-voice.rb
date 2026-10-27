cask "veyqa-voice" do
  version "0.3.0"
  sha256 "96532fb565d040fb2e1895ada256f86daaa170d0a727b91cbad4039129504911"

  url "https://github.com/wjc-dev/veyqa-voice/releases/download/v#{version}/Veyqa-Voice-v#{version}-macOS-arm64.dmg"
  name "Veyqa Voice"
  desc "Push-to-talk voice input"
  homepage "https://wjc-dev.github.io/veyqa-voice/"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Veyqa Voice.app"

  uninstall quit: ["com.wjcdev.veyqa", "com.wjcdev.veyqa.settings"]
end
