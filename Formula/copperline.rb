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
  url "https://github.com/CopperlineHQ/Copperline/archive/refs/tags/v0.18.0.tar.gz"
  sha256 "26f3802dca6de791021d60132f8e7ec585c753e69352f760d4ac306dc194995b"
  license "GPL-3.0-or-later"
  head "https://github.com/CopperlineHQ/Copperline.git", branch: "main"

  depends_on "rust" => :build

  # WHDLoad support archives for the direct WHDLoad boot (src/whdload.rs).
  # Both are freely redistributable and shipped unmodified; checksums are
  # pinned in step with tools/fetch-whdload.sh. whdload.de publishes
  # WHDLoad_usr.lha as a rolling latest-release file, so a new upstream
  # WHDLoad release means reviewing it and bumping the hash everywhere the
  # fetch script's header lists.
  resource "whdload" do
    url "https://whdload.de/whdload/WHDLoad_usr.lha", using: :nounzip
    sha256 "093333953737528d79c1eda7d21a16a0aa298698722624e7cfb31f588a0a156d"
  end

  resource "skick" do
    url "https://aminet.net/util/boot/skick346.lha", using: :nounzip
    sha256 "02b4d01852d12ab391c6469064f917221a0f7319fd0b3ba6c359403ec1d59f96"
  end

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

    # Install the bundled open-source lide.device autoboot ROM and
    # CD-filesystem bank (default for a fitted [lide] board without a named
    # rom/rom_bank2): <prefix>/share/copperline/lide.
    (pkgshare/"lide").install Dir["assets/lide/*"]

    # WHDLoad support archives: <prefix>/share/copperline/whdboot, where
    # whdload::find_whdboot_assets looks, with the provenance README beside
    # them. The stable formula can briefly point at a release made before a
    # newly added optional payload, while HEAD already contains it. Use the
    # source-tree marker so both revisions remain installable.
    if (buildpath/"assets/whdboot/README.md").exist?
      (pkgshare/"whdboot").install "assets/whdboot/README.md"
      resource("whdload").stage { (pkgshare/"whdboot").install "WHDLoad_usr.lha" }
      resource("skick").stage { (pkgshare/"whdboot").install "skick346.lha" }
    end
  end

  test do
    # --help prints usage to stderr and exits 0 without opening a window,
    # which proves the binary built and links against its GUI/audio stack.
    assert_match "Amiga emulator", shell_output("#{bin}/copperline --help 2>&1")
  end
end
