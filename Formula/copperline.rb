# Homebrew formula for Copperline.
#
# This repository doubles as its own Homebrew tap. Because it builds from
# source, the resulting binary is compiled on the user's machine and is not
# subject to macOS Gatekeeper quarantine -- no Security & Privacy override is
# ever needed. Install with:
#
#   brew tap copperlinehq/copperline https://github.com/CopperlineHQ/Copperline
#   brew install copperline
#
# or build the in-development tree directly:
#
#   brew install --HEAD copperline
#
# When tagging a release, update both `url` and `sha256` below. Compute the
# checksum from the tagged tarball:
#
#   curl -fsSL https://github.com/CopperlineHQ/Copperline/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
class Copperline < Formula
  desc "Cycle-driven Amiga emulator (OCS/ECS/AGA) written in Rust"
  homepage "https://copperline.dev/"
  url "https://github.com/CopperlineHQ/Copperline/archive/refs/tags/v0.15.0.tar.gz"
  sha256 "4954a0160574d99db98df19214166394fabffd8dea440e137f41c471c0599cbb"
  license "GPL-3.0-or-later"
  head "https://github.com/CopperlineHQ/Copperline.git", branch: "main"

  depends_on "rust" => :build

  def install
    # Cargo.lock is committed; std_cargo_args passes --locked so the build
    # uses the pinned dependency graph.
    system "cargo", "install", *std_cargo_args

    # Install the bundled AROS open-source Kickstart replacement (the default
    # boot ROM) where the binary looks for it: <prefix>/share/copperline/aros.
    # AROS is APL-licensed and freely redistributable, unlike a real Kickstart.
    (pkgshare/"aros").install Dir["assets/aros/*"]

    # Install the bundled open-source A4091 autoboot ROM (default when a config
    # fits an A4091 without naming a ROM): <prefix>/share/copperline/a4091.
    (pkgshare/"a4091").install Dir["assets/a4091/*"]
  end

  test do
    # --help prints usage to stderr and exits 0 without opening a window,
    # which proves the binary built and links against its GUI/audio stack.
    assert_match "Amiga emulator", shell_output("#{bin}/copperline --help 2>&1")
  end
end
