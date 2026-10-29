# Updated by .github/workflows/release.yml on each release (it opens a PR with the new url and sha256).
class TakeoutDl < Formula
  desc "Download Google Takeout archives with resume, retries and parallel parts"
  homepage "https://github.com/AndrewJack/google-takeout-downloader-cli"
  url "https://github.com/AndrewJack/google-takeout-downloader-cli/releases/download/v1.0.0/takeout-dl-v1.0.0-aarch64-apple-darwin.tar.gz"
  sha256 "6d9eac3e6f82c32c0032bfcdf55ad4b97152a063f4090cc7d221370ff9346620"
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
