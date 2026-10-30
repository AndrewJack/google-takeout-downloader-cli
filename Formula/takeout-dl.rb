# Updated by .github/workflows/release.yml on each release (it commits the new url and sha256).
class TakeoutDl < Formula
  desc "Download Google Takeout archives with resume, retries and parallel parts"
  homepage "https://github.com/AndrewJack/google-takeout-downloader-cli"
  url "https://github.com/AndrewJack/google-takeout-downloader-cli/releases/download/v1.4.0/takeout-dl-v1.4.0-aarch64-apple-darwin.tar.gz"
  sha256 "2cc57d896b6abf920d2cec10cda4b66f0c98f150285657d41185ea0b41bc493b"
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
