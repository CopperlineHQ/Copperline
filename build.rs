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
