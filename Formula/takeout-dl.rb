# Updated by .github/workflows/release.yml on each release (it commits the new url and sha256).
class TakeoutDl < Formula
  desc "Download Google Takeout archives with resume, retries and parallel parts"
  homepage "https://github.com/AndrewJack/google-takeout-downloader-cli"
  url "https://github.com/AndrewJack/google-takeout-downloader-cli/releases/download/v1.1.0/takeout-dl-v1.1.0-aarch64-apple-darwin.tar.gz"
  sha256 "2b9fa1da1e645089397e5b8445d25d4ce663babcc4c54a43ffccd2d966b6a0e2"
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
