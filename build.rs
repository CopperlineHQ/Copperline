use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let package_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let git_dir = git_output(["rev-parse", "--git-dir"]);
    let git_common_dir = git_output(["rev-parse", "--git-common-dir"]);
    if let Some(git_dir) = git_dir.as_deref() {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
    if let (Some(git_dir), Some(git_common_dir)) = (git_dir.as_deref(), git_common_dir.as_deref()) {
        if let Some(head_ref) = current_head_ref(git_dir) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
            println!("cargo:rerun-if-changed={git_common_dir}/{head_ref}");
        }
    }
    if let Some(git_common_dir) = git_common_dir.as_deref() {
        println!("cargo:rerun-if-changed={git_common_dir}/packed-refs");
        println!("cargo:rerun-if-changed={git_common_dir}/refs/tags");
    }

    embed_windows_resources(&package_version);

    let display_version = if exact_tagged_head() {
        package_version
    } else if let Some(short_hash) = git_output(["rev-parse", "--short=8", "HEAD"]) {
        format!("{package_version}+g{short_hash}")
    } else {
        package_version
    };

    println!("cargo:rustc-env=COPPERLINE_DISPLAY_VERSION={display_version}");

    generate_custom_register_docs();
    set_windows_main_thread_stack();
}

/// The executables the Windows packages ship, with the FileDescription each
/// one's version details carry and whether it wears the application icon.
/// Task Manager and "Open with" show the description as the program's name,
/// hence plain "Copperline" for the emulator itself. A binary added to the
/// ZIP needs an entry here; the Windows CI zip smoke checks every shipped
/// `.exe` has version details.
const WINDOWS_EXECUTABLES: &[(&str, &str, bool)] = &[
    ("copperline", "Copperline", true),
    ("copperline-ctl", "Copperline control client", false),
    (
        "copperline-import-uae",
        "Copperline UAE config importer",
        false,
    ),
];

/// Link Win32 resources into each shipped Windows executable: a VERSIONINFO
/// block, which is where Explorer's Properties > Details, Task Manager,
/// SmartScreen and code-signing services read the product name, publisher
/// and version; and in `copperline.exe` alone the application icon, which is
/// where Explorer, the taskbar, the Start menu and the Store package look for
/// it -- winit's `with_window_icon` dresses the window at runtime but says
/// nothing about the file on disk. The command-line tools shipped beside it
/// are not the app and should not wear its icon.
///
/// The scripts are generated into OUT_DIR so the version comes from Cargo,
/// not from a second copy that has to be kept in step at every release.
fn embed_windows_resources(package_version: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/brand/copperline.ico");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let icon_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/brand/copperline.ico");
    for &(bin, description, icon) in WINDOWS_EXECUTABLES {
        // The stem names the compiled resource file, so it must differ per
        // binary.
        let script = out_dir.join(format!("{bin}-resources.rc"));
        let icon = icon.then_some(icon_file.as_path());
        let source = windows_resource_script(bin, description, icon, package_version);
        std::fs::write(&script, source)
            .unwrap_or_else(|error| panic!("writing {}: {error}", script.display()));
        // Never fatal: a toolchain without a resource compiler should still
        // build a working program, just one with the default executable icon
        // and no version details.
        let result = embed_resource::compile_for(&script, [bin], embed_resource::NONE);
        if let Err(error) = result.manifest_optional() {
            println!("cargo:warning={bin}.exe resources not embedded: {error}");
        }
    }
}

/// The resource script for one executable. Numeric constants stand in for
/// the `winver.h` names (VOS_NT_WINDOWS32 = 0x40004, VFT_APP = 0x1,
/// VS_FFI_FILEFLAGSMASK = 0x3F) because not every resource compiler
/// embed-resource drives has the Windows SDK headers to hand.
fn windows_resource_script(
    bin: &str,
    description: &str,
    icon: Option<&Path>,
    version: &str,
) -> String {
    let numeric = windows_numeric_version(version);
    let mut script = String::new();
    if let Some(icon) = icon {
        // Resource id 1: the lowest icon id in a binary is the one Explorer,
        // the taskbar and the Start menu draw for it. This is the same
        // multi-size .ico the window itself loads at runtime, so the file on
        // disk and the window it opens agree. Named by absolute path, as
        // resource compilers differ on where they search for a bare name
        // (llvm-rc ignores include directories), and with forward slashes,
        // which every one of them accepts unescaped.
        let icon = icon.display().to_string().replace('\\', "/");
        writeln!(script, "1 ICON \"{icon}\"\n").expect("write to String");
    }
    // Id 1 is VS_VERSION_INFO. The strings keep the complete Cargo version,
    // prerelease suffix included, as the installer's version details do.
    write!(
        script,
        r#"1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "CopperlineHQ"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{bin}"
            VALUE "LegalCopyright", "Copperline is licensed under GPL-3.0-or-later."
            VALUE "OriginalFilename", "{bin}.exe"
            VALUE "ProductName", "Copperline"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
    .expect("write to String");
    script
}

/// `1.2.3-rc.1` -> `1,2,3,0`: a Windows file version is four 16-bit numbers,
/// with no room for the prerelease or build suffix only the strings carry.
fn windows_numeric_version(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts: Vec<u16> = core
        .split('.')
        .map(|part| {
            part.parse().unwrap_or_else(|_| {
                panic!("version {version}: {part:?} does not fit a Windows file version")
            })
        })
        .collect();
    parts.resize(4, 0);
    parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Compile the checked-in, human-readable register pages into the one table
/// consumed by the core debugger, CCP/DAP, console and editor extension.
fn generate_custom_register_docs() {
    let source = Path::new("docs/reference/custom-registers");
    println!("cargo:rerun-if-changed={}", source.display());
    let mut pages: Vec<PathBuf> = std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("reading {}: {error}", source.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && path.file_name().and_then(|name| name.to_str()) != Some("index.md")
        })
        .collect();
    pages.sort();
    let mut generated = String::from(
        "// Generated from docs/reference/custom-registers/*.md by build.rs.\n\
         pub static CUSTOM_REGISTER_DOCS: &[CustomRegisterDoc] = &[\n",
    );
    let mut previous = None;
    for path in pages {
        println!("cargo:rerun-if-changed={}", path.display());
        let markdown = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        // Git may materialise Markdown with CRLF on Windows.  Keep the
        // generated debugger text deterministic and make paragraph parsing
        // independent of the checkout's line-ending policy.
        let markdown = markdown.replace("\r\n", "\n").replace('\r', "\n");
        if !markdown.is_ascii() {
            panic!("{} must contain ASCII only", path.display());
        }
        let mut lines = markdown.lines();
        let name = lines
            .next()
            .and_then(|line| line.strip_prefix("# "))
            .unwrap_or_else(|| panic!("{}: first line must be '# NAME'", path.display()));
        let offset_text = lines
            .next()
            .and_then(|line| line.strip_prefix("Offset: $"))
            .unwrap_or_else(|| panic!("{}: second line must be 'Offset: $NNN'", path.display()));
        let offset = u16::from_str_radix(offset_text, 16)
            .unwrap_or_else(|_| panic!("{}: bad offset {offset_text:?}", path.display()));
        let access = lines
            .next()
            .and_then(|line| line.strip_prefix("Access: "))
            .unwrap_or_else(|| panic!("{}: third line must be 'Access: ...'", path.display()));
        let chipset = lines
            .next()
            .and_then(|line| line.strip_prefix("Chipset: "))
            .unwrap_or_else(|| panic!("{}: fourth line must be 'Chipset: ...'", path.display()));
        if previous.is_some_and(|old| offset <= old) {
            panic!(
                "{}: register pages must sort by increasing offset",
                path.display()
            );
        }
        previous = Some(offset);
        let summary = markdown
            .split("\n\n")
            .nth(1)
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .unwrap_or_else(|| panic!("{}: missing summary paragraph", path.display()));
        writeln!(
            generated,
            "    CustomRegisterDoc {{ offset: {offset:#05x}, name: {name:?}, access: {access:?}, chipset: {chipset:?}, summary: {summary:?}, markdown: {markdown:?} }},"
        )
        .expect("write to String");
    }
    generated.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("custom_register_docs.rs");
    std::fs::write(&out, generated)
        .unwrap_or_else(|error| panic!("writing {}: {error}", out.display()));
}

/// Give the Windows binaries the same ~8 MiB main-thread stack that Linux and
/// macOS provide by default; the MSVC linker otherwise reserves only 1 MiB.
///
/// The winit event loop must run on the main thread, and the configuration
/// screen's Run boots the machine from inside that event-loop callback -- so the
/// machine build, the render-state rebuild, and the first present all run deep in
/// the OS message-pump stack. That bounded-but-substantial work overflows the
/// 1 MiB default (a silent exit / STATUS_STACK_OVERFLOW when the user clicks Run),
/// while it fits comfortably in the 8 MiB the other platforms already give. Scoped
/// to binary targets and, via the target cfg, to Windows MSVC builds only.
fn set_windows_main_thread_stack() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        const STACK_BYTES: usize = 8 * 1024 * 1024;
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
    }
}

fn current_head_ref(git_dir: &str) -> Option<String> {
    let head = std::fs::read_to_string(Path::new(git_dir).join("HEAD")).ok()?;
    head.strip_prefix("ref: ").map(|s| s.trim().to_string())
}

fn exact_tagged_head() -> bool {
    git_output(["describe", "--tags", "--exact-match", "HEAD"]).is_some()
}

fn git_output<const N: usize>(args: [&str; N]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
