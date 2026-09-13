# Microsoft Store package

Copperline ships to the Microsoft Store as an MSIX bundle: `copperline.exe`
and its companions packaged as a Win32 full-trust app, with both
architectures in one file so a single submission serves x64 and ARM64
machines.

The package contents are exactly the portable zip's payload. Both come from
`../stage-payload.ps1`, so the bundled AROS ROM, the FMV, A2091, A4091, LIDE
and HRTMon ROMs, and the WHDLoad support archives sit beside the executable
in the package the same way they sit beside it in the zip, and
`romsearch.rs` finds them with no configuration.

## Identity

`identity.psd1` holds the three values Partner Center issues, copied from
**Product management -> Product identity**. They are case-sensitive; a
mismatch is rejected at upload with an identity error rather than a useful
message. Nothing here is a credential: a package identity is public in every
installed copy, and the Store applies the signature.

## What the Store signs

Store submissions are **unsigned on the way in**. Partner Center replaces any
signature with a Microsoft certificate during certification, which is why
this route needs no code-signing certificate at all. (The EXE/MSI submission
route is the opposite: it needs an Authenticode certificate from a CA in the
Microsoft Trusted Root Program, and the installer hosted at a URL of your
own.)

A bundle downloaded from CI therefore installs nowhere as-is. Use `-SelfSign`
to test one locally.

## Building

CI does this on every push in the `Microsoft Store package` job of
`.github/workflows/windows.yml`, which packs the payloads the release zips
were built from and uploads `copperline-msixbundle` as a workflow artifact.
Download that artifact to submit it.

To build one by hand on a Windows machine with the Windows SDK installed:

```powershell
# From already-staged payloads (what CI does):
packaging\windows\msix\build-msix.ps1 -Payload x64=C:\stage\x64, arm64=C:\stage\arm64

# Or build and stage from source, and sign it so it can be installed:
packaging\windows\msix\build-msix.ps1 -Architecture x64 -SelfSign
```

Testing a self-signed bundle needs its certificate trusted first, which is a
per-machine change to undo afterwards:

```powershell
Import-Certificate -FilePath Copperline-test-signing.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage Copperline-1.20.0.0.msixbundle
```

Run the [Windows App Certification
Kit](https://learn.microsoft.com/en-us/windows/uwp/debug-test-perf/windows-app-certification-kit)
against the bundle before the first submission of a release.

## The listing

`store-listing.md` holds the Store listing text -- description, feature
bullets, licence terms, search terms, and the screenshot and age-rating
notes -- so it is versioned with the package rather than living only in
Partner Center. Two constraints shape it: the listing must never suggest
that copyrighted ROMs or software come with the emulator, and it must not
imply a relationship with the Amiga trademark.

## Version numbers

The Store constrains package versions in two ways that the crate version does
not satisfy: the fourth part is reserved and must be `0`, and the first part
cannot be `0`. `build-msix.ps1` therefore shifts the major up by one, so
`0.20.0` ships as `1.20.0.0` and an eventual `1.0.0` ships as `2.0.0.0` --
strictly increasing across the 0.x boundary, which is what the Store needs in
order to treat a submission as an update. The Store listing shows that
shifted number. Pass `-Version` to override it.

## Regenerating the logos

`assets/` holds the tile and icon PNGs the manifest names, generated from the
repository's brand artwork:

```sh
python3 packaging/windows/msix/generate-logos.py
```

## Known gaps

- **No file type associations.** The emulator parses a bare command-line path
  as a Kickstart ROM (`src/cli.rs`), so associating `.adf` and friends would
  make a double-clicked disk image fail rather than boot. Declare them once a
  positional path is dispatched by what the file actually is.
- **Raw floppy and hard-disk device access is unavailable.** That path
  elevates by re-launching the executable (`src/blockdev/windows.rs`), and a
  packaged app cannot be launched elevated from its install directory. Disk
  images and directory-as-hard-disk are unaffected.
- **The command-line companions are not on PATH.** They ship inside the
  package, but aliasing them needs one `Application` element each, and
  hiding those with `AppListEntry="none"` is what Partner Center calls a
  headless app: it is refused without a `HeadlessAppBypass` waiver. Listing
  them visibly would put two console tools in the Start menu instead. The
  portable zip remains the way to get the command-line tooling.
- **Portable mode cannot apply.** A package installs read-only, so the
  `portable.txt` marker beside the executable is never writable. Host data
  goes to `Documents\Copperline` as it does for any non-portable install.
