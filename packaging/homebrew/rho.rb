# Homebrew formula for rho — a minimalist Pi-style coding-agent harness
# (Rust port of tau). Installs the `rho` command.
#
# This is the in-repo TEMPLATE for the tap repo `ramanshrivastava/homebrew-tap`.
# Copy it there as `Formula/rho.rb` so users can run:
#
#     brew install ramanshrivastava/tap/rho
#
# The GitHub release artifacts it points at are produced by
# `.github/workflows/release.yml` (cargo-dist). The crates.io package is named
# `rho-code` (the bare `rho` name is squatted), which is why the tarballs are
# named `rho-code-<target>.tar.xz` — but the binary inside, and the Homebrew
# formula/command, are both `rho`.
#
# ── Per-release update procedure ─────────────────────────────────────────────
# On each new vX.Y.Z GitHub release:
#   1. Bump `version` below to the new X.Y.Z.
#   2. Replace every `sha256` with the real digest of that release's tarball.
#      cargo-dist publishes a `sha256.sum` artifact on the release; or compute
#      them directly, e.g.:
#        curl -sL https://github.com/ramanshrivastava/rho/releases/download/vX.Y.Z/rho-code-aarch64-apple-darwin.tar.xz | shasum -a 256
#   3. Commit the updated formula to the tap repo. Then
#      `brew update && brew upgrade rho` picks it up.
#
# The `url`s use `#{version}`, so step 1 is the only edit to the URLs.
#
# AUTOMATION ALTERNATIVE: cargo-dist can generate and push this formula itself.
# Set in dist-workspace.toml:
#     [dist]
#     tap = "ramanshrivastava/homebrew-tap"
#     publish-jobs = ["homebrew"]
# and add a HOMEBREW_TAP_TOKEN repo secret (a PAT with write access to the tap).
# The release workflow then regenerates + pushes the formula on every tag,
# making steps 1–3 automatic. This checked-in file is the manual fallback and
# the source of truth for the artifact layout.
# ─────────────────────────────────────────────────────────────────────────────

class Rho < Formula
  desc "Minimalist Pi-style coding-agent harness (Rust port of tau)"
  homepage "https://github.com/ramanshrivastava/rho"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/ramanshrivastava/rho/releases/download/v#{version}/rho-code-aarch64-apple-darwin.tar.xz"
      # PLACEHOLDER — replace with the real digest before publishing the tap.
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/ramanshrivastava/rho/releases/download/v#{version}/rho-code-x86_64-apple-darwin.tar.xz"
      # PLACEHOLDER — replace with the real digest before publishing the tap.
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/ramanshrivastava/rho/releases/download/v#{version}/rho-code-x86_64-unknown-linux-gnu.tar.xz"
      # PLACEHOLDER — replace with the real digest before publishing the tap.
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    # cargo-dist tarballs are flat: the `rho` binary sits at the archive root.
    bin.install "rho"
  end

  test do
    assert_match "0.1.0", shell_output("#{bin}/rho --version")
  end
end
