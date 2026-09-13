// SPDX-License-Identifier: GPL-3.0-or-later

//! Putting away the console window a desktop launch brings with it (Windows).
//!
//! `copperline` is one binary doing two jobs: the windowed emulator and a
//! command-line tool (`--screenshot-after`, `--help`, every headless flag).
//! The console subsystem is what the command-line half needs -- a shell waits
//! for a console-subsystem process and its output lands in the terminal, where
//! a GUI-subsystem one hands the prompt straight back and prints nowhere
//! anybody is looking, which would break scripted and CI runs. Windows,
//! though, also hands a console-subsystem process launched from Explorer, the
//! Start menu or the Store package a console window of its own, and that one
//! has no business sitting behind the emulator.
//!
//! So the subsystem stays as it is and the window goes instead -- but only
//! when it is ours to close. `GetConsoleProcessList` reporting this process
//! alone means the console was made for this launch: any shell that started us
//! would still be attached to it, as would `cargo run`, and those keep their
//! console and every byte Copperline writes to it.
//!
//! The standard handles are pointed at `NUL` before the console goes. Rust
//! fetches them per write, so a `println!` after `FreeConsole` would otherwise
//! write to a closed handle, and `println!` panics when the write fails.

/// Close the console window Windows created for a desktop launch. A no-op
/// everywhere else, and on Windows whenever the console belongs to a shell.
///
/// Call once, before anything writes to stdout or stderr.
#[cfg(not(windows))]
pub fn detach_desktop_console() {}

#[cfg(windows)]
pub fn detach_desktop_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        FreeConsole, GetConsoleProcessList, GetConsoleWindow, SetStdHandle, STD_ERROR_HANDLE,
        STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    // SAFETY: plain queries about this process's own console, and handle
    // swaps on it, all before any thread of ours has written to one.
    unsafe {
        let console = GetConsoleWindow();
        if console.is_null() {
            // No console at all: started by something windowed, or already
            // detached. Nothing to do either way.
            return;
        }
        // Room for two so a shared console is told apart from ours; the count
        // is what is wanted, not the list.
        let mut attached = [0u32; 2];
        if GetConsoleProcessList(attached.as_mut_ptr(), attached.len() as u32) != 1 {
            // Somebody else is attached -- a shell, or cargo run. Their
            // console, and Copperline's output belongs in it.
            return;
        }
        // Hide before freeing: the window was on screen before this process
        // got to run, and this is the earliest the flash can be cut short.
        ShowWindow(console, SW_HIDE);

        let nul: Vec<u16> = "NUL\0".encode_utf16().collect();
        let handles: [(STD_HANDLE, u32); 3] = [
            (STD_INPUT_HANDLE, GENERIC_READ),
            (STD_OUTPUT_HANDLE, GENERIC_WRITE),
            (STD_ERROR_HANDLE, GENERIC_WRITE),
        ];
        for (id, access) in handles {
            let file = CreateFileW(
                nul.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if file != INVALID_HANDLE_VALUE {
                SetStdHandle(id, file);
            }
        }
        FreeConsole();
    }
}
