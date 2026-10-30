# Updated by .github/workflows/release.yml on each release (it commits the new url and sha256).
class TakeoutDl < Formula
  desc "Download Google Takeout archives with resume, retries and parallel parts"
  homepage "https://github.com/AndrewJack/google-takeout-downloader-cli"
  url "https://github.com/AndrewJack/google-takeout-downloader-cli/releases/download/v1.3.0/takeout-dl-v1.3.0-aarch64-apple-darwin.tar.gz"
  sha256 "9a864204ffae8406bed22e92056650d65980c50fb5f3c070657edb00d97d29b7"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "takeout-dl"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/takeout-dl --version")
  end
end
