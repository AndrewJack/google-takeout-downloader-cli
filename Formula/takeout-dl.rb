# Updated by .github/workflows/release.yml on each release (it commits the new url and sha256).
class TakeoutDl < Formula
  desc "Download Google Takeout archives with resume, retries and parallel parts"
  homepage "https://github.com/AndrewJack/google-takeout-downloader-cli"
  url "https://github.com/AndrewJack/google-takeout-downloader-cli/releases/download/v1.2.0/takeout-dl-v1.2.0-aarch64-apple-darwin.tar.gz"
  sha256 "be62cf1c511c1c43d0781819ec12dad48f333687868140e05425b78a2fa2329e"
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
