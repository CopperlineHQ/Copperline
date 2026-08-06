// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS block devices, through IOKit.
//!
//! Devices are discovered from the IO registry rather than by reading
//! `/dev`, because the registry carries what a user needs to choose by --
//! model, capacity, which bus it hangs off -- and because listing it opens
//! nothing. A sleeping drive stays asleep, and no privilege is needed: the
//! prompt comes later, only for the device actually chosen.
//!
//! Two details of the registry shape the code. Whole disks are `IOMedia`
//! objects with `Whole` set, which is what we want and what excludes both
//! partitions and the synthesized APFS containers that `diskutil` shows as
//! separate disks. But the model name and whether the bus is internal live
//! on an *ancestor* of the media object, the block-storage device, so those
//! are fetched by searching up the service plane rather than read directly.
//!
//! Mounted volumes come from `getfsstat`, matching each mount's device
//! against the disk it belongs to. That is also how the running system's own
//! disk is identified: whatever serves `/` is the one device that must never
//! be offered.

use std::ffi::{CStr, CString};
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{HostDevice, Safety};

// IOKit and CoreFoundation. Only the dozen calls this needs are declared;
// the alternative is a framework binding crate for one screen of FFI.
#[allow(non_camel_case_types)]
type io_object_t = u32;
#[allow(non_camel_case_types)]
type io_iterator_t = io_object_t;
#[allow(non_camel_case_types)]
type io_registry_entry_t = io_object_t;
#[allow(non_camel_case_types)]
type mach_port_t = u32;
#[allow(non_camel_case_types)]
type kern_return_t = i32;

type CFTypeRef = *const std::ffi::c_void;
type CFStringRef = CFTypeRef;
type CFDictionaryRef = CFTypeRef;
type CFMutableDictionaryRef = *mut std::ffi::c_void;
type CFAllocatorRef = CFTypeRef;
type CFTypeID = usize;
type CFIndex = isize;

const KERN_SUCCESS: kern_return_t = 0;
/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// `kIORegistryIterateRecursively | kIORegistryIterateParents`.
const SEARCH_PARENTS: u32 = 1 | 2;
/// `kCFNumberSInt64Type`.
const CF_NUMBER_S64: CFIndex = 4;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    static kIOMainPortDefault: mach_port_t;
    fn IOServiceMatching(name: *const libc::c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        port: mach_port_t,
        matching: CFMutableDictionaryRef,
        existing: *mut io_iterator_t,
    ) -> kern_return_t;
    fn IOIteratorNext(iterator: io_iterator_t) -> io_object_t;
    fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    fn IORegistryEntryCreateCFProperties(
        entry: io_registry_entry_t,
        properties: *mut CFMutableDictionaryRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> kern_return_t;
    fn IORegistryEntrySearchCFProperty(
        entry: io_registry_entry_t,
        plane: *const libc::c_char,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFStringCreateWithCString(
        allocator: CFAllocatorRef,
        cstr: *const libc::c_char,
        encoding: u32,
    ) -> CFStringRef;
    // CoreFoundation's `Boolean` is an unsigned char, not Rust's bool.
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut libc::c_char,
        size: CFIndex,
        encoding: u32,
    ) -> u8;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
    fn CFNumberGetValue(number: CFTypeRef, the_type: CFIndex, value: *mut i64) -> u8;
}

// Security.framework: enough to ask for one right ourselves, so the prompt
// is presented on Copperline's behalf rather than by the tool it drives.
#[allow(non_camel_case_types)]
type AuthorizationRef = *mut std::ffi::c_void;
type OSStatus = i32;

/// `AuthorizationExternalForm`: an opaque 32-byte blob by definition.
const AUTHORIZATION_EXTERNAL_FORM_LENGTH: usize = 32;

#[repr(C)]
struct AuthorizationItem {
    name: *const libc::c_char,
    value_length: usize,
    value: *mut std::ffi::c_void,
    flags: u32,
}

#[repr(C)]
struct AuthorizationRights {
    count: u32,
    items: *mut AuthorizationItem,
}

/// `kAuthorizationFlagInteractionAllowed | kAuthorizationFlagExtendRights`.
const AUTH_FLAGS_ASK: u32 = (1 << 0) | (1 << 1);

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(
        rights: *const AuthorizationRights,
        environment: *const std::ffi::c_void,
        flags: u32,
        authorization: *mut AuthorizationRef,
    ) -> OSStatus;
    fn AuthorizationCopyRights(
        authorization: AuthorizationRef,
        rights: *const AuthorizationRights,
        environment: *const std::ffi::c_void,
        flags: u32,
        authorized_rights: *mut *mut AuthorizationRights,
    ) -> OSStatus;
    fn AuthorizationMakeExternalForm(
        authorization: AuthorizationRef,
        external_form: *mut u8,
    ) -> OSStatus;
    fn AuthorizationFree(authorization: AuthorizationRef, flags: u32) -> OSStatus;
}

/// Ask the user for the right to open one device, as Copperline.
///
/// `authopen` will present its own dialog if left to it, and that dialog
/// names `authopen` -- which tells the user nothing about who is asking or
/// why. Building the authorization here instead means the prompt is raised by
/// this process, so it is attributed to Copperline, and `authopen` is handed
/// the result rather than asking again.
///
/// Returns the external form to write to its stdin, or `None` if the user
/// declined or the request failed -- in which case the caller lets `authopen`
/// ask in its own name rather than failing outright.
fn authorize_open(path: &str, write: bool) -> Option<[u8; AUTHORIZATION_EXTERNAL_FORM_LENGTH]> {
    // The right is per-path, which is what keeps a granted open from being
    // turned on any other device.
    let right = CString::new(format!(
        "sys.openfile.{}.{path}",
        if write { "readwrite" } else { "readonly" }
    ))
    .ok()?;
    let mut item = AuthorizationItem {
        name: right.as_ptr(),
        value_length: 0,
        value: std::ptr::null_mut(),
        flags: 0,
    };
    let rights = AuthorizationRights {
        count: 1,
        items: &mut item,
    };

    let mut auth: AuthorizationRef = std::ptr::null_mut();
    // Created empty, then extended: the dialog belongs to the second call, so
    // a failure to create is distinguishable from a refusal to grant.
    if unsafe { AuthorizationCreate(std::ptr::null(), std::ptr::null(), 0, &mut auth) } != 0 {
        return None;
    }
    let granted = unsafe {
        AuthorizationCopyRights(
            auth,
            &rights,
            std::ptr::null(),
            AUTH_FLAGS_ASK,
            std::ptr::null_mut(),
        )
    };
    if granted != 0 {
        unsafe { AuthorizationFree(auth, 0) };
        return None;
    }
    let mut external = [0u8; AUTHORIZATION_EXTERNAL_FORM_LENGTH];
    let made = unsafe { AuthorizationMakeExternalForm(auth, external.as_mut_ptr()) };
    unsafe { AuthorizationFree(auth, 0) };
    (made == 0).then_some(external)
}

/// A CoreFoundation object this code owns and must release.
struct CfOwned(CFTypeRef);

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// An IOKit object this code owns and must release.
struct IoOwned(io_object_t);

impl Drop for IoOwned {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { IOObjectRelease(self.0) };
        }
    }
}

/// A short-lived CFString for a literal key.
fn cfstr(value: &str) -> Option<CfOwned> {
    let c = CString::new(value).ok()?;
    let s = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) };
    (!s.is_null()).then_some(CfOwned(s))
}

/// Read a CFString value out of a dictionary.
fn dict_string(dict: CFDictionaryRef, key: &str) -> Option<String> {
    let key = cfstr(key)?;
    let value = unsafe { CFDictionaryGetValue(dict, key.0) };
    read_string(value)
}

fn read_string(value: CFTypeRef) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    // Long enough for any model or BSD name; a longer one is simply not read
    // rather than truncated into something misleading.
    let mut buffer = [0 as libc::c_char; 256];
    let ok =
        unsafe { CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as CFIndex, UTF8) };
    if ok == 0 {
        return None;
    }
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn dict_bool(dict: CFDictionaryRef, key: &str) -> Option<bool> {
    let key = cfstr(key)?;
    let value = unsafe { CFDictionaryGetValue(dict, key.0) };
    if value.is_null() || unsafe { CFGetTypeID(value) } != unsafe { CFBooleanGetTypeID() } {
        return None;
    }
    Some(unsafe { CFBooleanGetValue(value) } != 0)
}

fn dict_number(dict: CFDictionaryRef, key: &str) -> Option<i64> {
    let key = cfstr(key)?;
    let value = unsafe { CFDictionaryGetValue(dict, key.0) };
    if value.is_null() || unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut out: i64 = 0;
    (unsafe { CFNumberGetValue(value, CF_NUMBER_S64, &mut out) } != 0).then_some(out)
}

/// Search this object and its ancestors for a property.
///
/// The model name and the bus location belong to the block-storage device,
/// which sits above the media object in the registry, so they cannot be read
/// from the media's own properties.
fn inherited(entry: io_registry_entry_t, key: &str) -> Option<CfOwned> {
    let key = cfstr(key)?;
    let plane = CString::new("IOService").ok()?;
    let value = unsafe {
        IORegistryEntrySearchCFProperty(
            entry,
            plane.as_ptr(),
            key.0,
            std::ptr::null(),
            SEARCH_PARENTS,
        )
    };
    (!value.is_null()).then_some(CfOwned(value))
}

/// A property of the device dictionary that hangs off an ancestor, e.g. the
/// product name inside "Device Characteristics".
fn inherited_dict_string(entry: io_registry_entry_t, dict_key: &str, key: &str) -> Option<String> {
    let dict = inherited(entry, dict_key)?;
    if unsafe { CFGetTypeID(dict.0) } != unsafe { CFDictionaryGetTypeID() } {
        return None;
    }
    dict_string(dict.0, key)
}

/// Mount points the host currently serves, paired with the whole-disk
/// identifier they come from.
///
/// `f_mntfromname` is the slice (`/dev/disk3s5`), so the leading `diskN` is
/// taken as the disk it belongs to.
fn mounted_volumes() -> Vec<(String, String)> {
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let mut buffer: Vec<libc::statfs> = Vec::with_capacity(count as usize);
    let bytes = count * std::mem::size_of::<libc::statfs>() as i32;
    let filled = unsafe { libc::getfsstat(buffer.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if filled <= 0 {
        return Vec::new();
    }
    unsafe { buffer.set_len(filled as usize) };
    buffer
        .iter()
        .filter_map(|fs| {
            let from = unsafe { CStr::from_ptr(fs.f_mntfromname.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let on = unsafe { CStr::from_ptr(fs.f_mntonname.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let slice = from.strip_prefix("/dev/")?;
            Some((whole_disk_of(slice), on))
        })
        .collect()
}

/// The whole-disk name a slice belongs to: `disk3s5` and `disk3s1s1` are both
/// `disk3`.
fn whole_disk_of(slice: &str) -> String {
    match slice.find('s') {
        // Skip the "disk" prefix before looking for the slice separator, so
        // the 's' in "disk" is not mistaken for one.
        Some(_) => {
            let (head, tail) = slice.split_at(4.min(slice.len()));
            match tail.find('s') {
                Some(at) => format!("{head}{}", &tail[..at]),
                None => slice.to_string(),
            }
        }
        None => slice.to_string(),
    }
}

/// Every whole device in the IO registry.
pub fn list_devices() -> Result<Vec<HostDevice>> {
    let matching = unsafe {
        let class = CString::new("IOMedia").expect("literal has no NUL");
        IOServiceMatching(class.as_ptr())
    };
    if matching.is_null() {
        anyhow::bail!("IOServiceMatching(IOMedia) returned nothing");
    }
    // IOServiceGetMatchingServices consumes the dictionary reference.
    let mut iterator: io_iterator_t = 0;
    let rc = unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, matching, &mut iterator) };
    if rc != KERN_SUCCESS {
        anyhow::bail!("IOServiceGetMatchingServices failed: {rc}");
    }
    let iterator = IoOwned(iterator);

    let mounts = mounted_volumes();
    // Whatever serves the root filesystem is the running system's disk.
    let system_disk = mounts
        .iter()
        .find(|(_, on)| on == "/")
        .map(|(disk, _)| disk.clone());

    let mut devices = Vec::new();
    loop {
        let media = unsafe { IOIteratorNext(iterator.0) };
        if media == 0 {
            break;
        }
        let media = IoOwned(media);
        if let Some(device) = describe(media.0, &mounts, system_disk.as_deref()) {
            devices.push(device);
        }
    }
    Ok(devices)
}

/// Turn one `IOMedia` object into a device, or nothing if it is not a whole
/// disk worth offering.
fn describe(
    media: io_registry_entry_t,
    mounts: &[(String, String)],
    system_disk: Option<&str>,
) -> Option<HostDevice> {
    let mut raw: CFMutableDictionaryRef = std::ptr::null_mut();
    let rc = unsafe { IORegistryEntryCreateCFProperties(media, &mut raw, std::ptr::null(), 0) };
    if rc != KERN_SUCCESS || raw.is_null() {
        return None;
    }
    let properties = CfOwned(raw.cast_const());
    let dict = properties.0;

    // Whole disks only. Partitions and the synthesized containers macOS
    // builds over them are not media anyone can hand to an Amiga.
    if !dict_bool(dict, "Whole").unwrap_or(false) {
        return None;
    }
    let id = dict_string(dict, "BSD Name")?;
    let size_bytes = dict_number(dict, "Size").unwrap_or(0) as u64;
    if size_bytes == 0 {
        return None;
    }
    let block_size = dict_number(dict, "Preferred Block Size").unwrap_or(512) as u32;
    let removable = dict_bool(dict, "Removable").unwrap_or(false);
    let ejectable = dict_bool(dict, "Ejectable").unwrap_or(false);
    let writable = dict_bool(dict, "Writable").unwrap_or(true);

    let model = inherited_dict_string(media, "Device Characteristics", "Product Name");
    let internal = inherited(media, "Physical Interconnect Location")
        .and_then(|value| read_string(value.0))
        .map(|location| location == "Internal")
        .unwrap_or(false);

    let mounted: Vec<String> = mounts
        .iter()
        .filter(|(disk, _)| *disk == id)
        .map(|(_, on)| on.clone())
        .collect();

    let safety = if system_disk == Some(id.as_str()) {
        Safety::SystemDisk
    } else if internal && !removable && !ejectable {
        Safety::Internal
    } else {
        Safety::Offerable
    };

    Some(HostDevice {
        // The unbuffered character device: transfers go straight to the
        // caller's buffer instead of through the buffer cache, which is what
        // bulk disk access wants.
        path: PathBuf::from(format!("/dev/r{id}")),
        id,
        model,
        size_bytes,
        block_size,
        removable: removable || ejectable,
        internal,
        writable,
        mounted,
        safety,
    })
}

/// Take a disk away from the host so the guest can have it exclusively.
///
/// An Amiga-formatted disk usually is not mounted at all -- macOS cannot read
/// the filesystem, and offers to initialise it instead, which is the offer to
/// decline. But a disk that carries anything macOS *does* recognise will be
/// mounted, and the kernel holds the media while a volume is mounted from it,
/// so opening the whole disk for writing fails with `EBUSY` until every
/// volume on it is gone. This is the step a user otherwise has to know to do
/// by hand (`diskutil unmountDisk /dev/diskN`), which is exactly the sort of
/// thing an emulator should do for them.
///
/// `diskutil` is used rather than the DiskArbitration API directly: it is the
/// supported interface, it already resolves the whole-disk relationship, and
/// it needs no entitlement -- so it keeps working unchanged inside a signed,
/// notarized application bundle, where a hand-rolled unmount would be one
/// more thing to get past the sandbox and the notary.
pub fn unmount_whole_disk(id: &str) -> Result<()> {
    let output = std::process::Command::new("/usr/sbin/diskutil")
        .arg("unmountDisk")
        .arg(format!("/dev/{id}"))
        .output()
        .context("running diskutil unmountDisk")?;
    if output.status.success() {
        log::info!("blockdev: {id} unmounted; the machine has it exclusively");
        return Ok(());
    }
    // Nothing mounted is a success as far as we are concerned: the disk is
    // already ours to open, which is the state this was asked to reach.
    let message = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{message}", String::from_utf8_lossy(&output.stdout));
    if combined.contains("was already unmounted") || combined.contains("Unmount successful") {
        return Ok(());
    }
    anyhow::bail!("{id} could not be unmounted: {}", combined.trim());
}

/// Open a device, asking the system for permission only if it has to.
///
/// Two things stand between a process and `/dev/rdiskN`, and they are
/// independent. The node is `root:operator 0640`, and macOS additionally
/// gates raw media behind a privacy check ("Removable Volumes"), which is why
/// a denial here arrives as `EPERM` rather than the `EACCES` a plain
/// permission failure would give. **Being root does not satisfy the second
/// one**: a root daemon of our own is refused just as an ordinary process is.
///
/// `/usr/libexec/authopen` is Apple's answer to exactly this. It is setuid
/// root, ships with the system, shows the standard authorization prompt, and
/// is entitled to have the privacy check attributed to *us* rather than to
/// itself -- an entitlement reserved to Apple, so no helper we could write
/// would be allowed to do the same. It opens the device and passes the
/// descriptor back, then exits; the descriptor keeps working afterwards,
/// because permission is decided once, at open, and travels with it.
///
/// A direct open is tried first: media that already belongs to the user (a
/// card reader on some setups, an attached image) needs no prompt at all, and
/// asking when nothing is in the way would be rude.
pub fn open_device(device: &HostDevice, write: bool) -> Result<super::BlockDevice> {
    let path = device.path.to_str().context("device path is not UTF-8")?;
    // O_EXCL on a device node claims the media, which stops the system
    // mounting a volume underneath the emulator mid-session. Without it the
    // claim is advisory only.
    let flags = if write {
        libc::O_RDWR | libc::O_EXCL
    } else {
        libc::O_RDONLY
    };

    // The host must let go before the machine can take hold: a mounted volume
    // keeps the kernel's claim on the media and the open below would fail.
    if !device.mounted.is_empty() {
        unmount_whole_disk(&device.id)?;
    }
    let file = match direct_open(path, flags) {
        Ok(file) => file,
        Err(error) => {
            let denied = matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES));
            if !denied {
                return Err(error).with_context(|| format!("opening {path}"));
            }
            log::info!(
                "blockdev: {} needs permission; asking through authopen",
                device.id
            );
            authopen(path, flags).with_context(|| format!("opening {path} with authorization"))?
        }
    };

    Ok(super::BlockDevice::new(
        file,
        device.id.clone(),
        device.block_size,
        device.size_bytes,
        write,
    ))
}

fn direct_open(path: &str, flags: i32) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let c_path = CString::new(path)?;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Ask `authopen` to open the device and hand the descriptor back.
///
/// The descriptor arrives as ancillary data on a socket standing in for the
/// tool's standard output, which is what its `-stdoutpipe` mode expects.
fn authopen(path: &str, flags: i32) -> Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    // Deliberately `-o <flags>` and never `-w`: the `-w` mode truncates the
    // file it opens, which on a real disk would destroy it.
    let mut pair = [0; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("socketpair for authopen");
    }
    let ours = unsafe { OwnedFd::from_raw_fd(pair[0]) };
    let theirs = unsafe { OwnedFd::from_raw_fd(pair[1]) };

    // Ask for the right ourselves so the prompt is raised by, and named
    // after, Copperline. If that does not happen -- the user declines, or the
    // request fails -- fall back to letting the tool ask in its own name
    // rather than refusing to open at all.
    let external = authorize_open(path, flags != libc::O_RDONLY);
    let mut command = std::process::Command::new("/usr/libexec/authopen");
    command.arg("-stdoutpipe");
    if external.is_some() {
        command.arg("-extauth");
    }
    command
        .arg("-o")
        .arg(flags.to_string())
        .arg(path)
        .stdout(std::process::Stdio::from(theirs));
    if external.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let mut child = command.spawn().context("running /usr/libexec/authopen")?;
    // The authorization is read before anything else on stdin.
    if let Some(form) = external {
        use std::io::Write;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(&form)
                .context("handing over the authorization")?;
            stdin.flush().ok();
        }
        drop(child.stdin.take());
    }

    let received = receive_fd(ours.as_raw_fd());
    let status = child.wait().context("waiting for authopen")?;
    let fd = received?;
    if !status.success() {
        anyhow::bail!("authorization was refused");
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Receive a single descriptor as ancillary data.
///
/// Written against Darwin's rules rather than Linux's, which differ in ways
/// that fail silently: the control length must equal the message length
/// exactly (slack is rejected outright), the header's length field is 32-bit,
/// and `SOL_SOCKET` has a different value. Truncation is treated as fatal
/// because this kernel installs every descriptor in the message before
/// truncating what it reports, so a short buffer leaks them.
fn receive_fd(socket: libc::c_int) -> Result<libc::c_int> {
    #[repr(C)]
    struct CmsgHeader {
        len: u32,
        level: i32,
        kind: i32,
    }
    const HEADER: usize = std::mem::size_of::<CmsgHeader>();
    /// Darwin aligns control data to 4 bytes, not to a pointer.
    const fn align(n: usize) -> usize {
        (n + 3) & !3
    }
    /// Darwin's `SOL_SOCKET`.
    const SOL_SOCKET: i32 = 0xffff;

    let space = align(HEADER) + align(std::mem::size_of::<libc::c_int>());
    let mut control = vec![0u8; space];
    // authopen writes a byte alongside the descriptor; a message with no
    // ordinary data would arrive as a zero-length read, which cannot be told
    // apart from end of file.
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&mut byte as *mut u8).cast(),
        iov_len: 1,
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as libc::socklen_t;

    let received = unsafe { libc::recvmsg(socket, &mut message, 0) };
    if received < 0 {
        return Err(std::io::Error::last_os_error()).context("receiving the descriptor");
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        anyhow::bail!("the descriptor did not fit in the control buffer");
    }
    if (message.msg_controllen as usize) < align(HEADER) + std::mem::size_of::<libc::c_int>() {
        anyhow::bail!("no descriptor came back (authorization refused?)");
    }
    let header = unsafe { &*control.as_ptr().cast::<CmsgHeader>() };
    if header.level != SOL_SOCKET || header.kind != libc::SCM_RIGHTS {
        anyhow::bail!("the reply carried something other than a descriptor");
    }
    let fd = unsafe {
        std::ptr::read_unaligned(control.as_ptr().add(align(HEADER)).cast::<libc::c_int>())
    };
    if fd < 0 {
        anyhow::bail!("the descriptor that came back is not valid");
    }
    // Close it on exec. This is a descriptor to raw media opened with
    // privileges we do not otherwise hold, and any process this one spawns
    // would otherwise inherit it. Darwin has no way to ask for the flag as
    // part of receiving, so it is set immediately afterwards.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice belongs to the disk it is cut from, however many levels deep
    /// the name goes -- APFS snapshots reach `disk3s1s1`.
    #[test]
    fn a_slice_maps_to_its_whole_disk() {
        assert_eq!(whole_disk_of("disk3s5"), "disk3");
        assert_eq!(whole_disk_of("disk3s1s1"), "disk3");
        assert_eq!(whole_disk_of("disk0"), "disk0");
        assert_eq!(whole_disk_of("disk12s1"), "disk12");
    }

    /// Enumeration must work with no privileges and no hardware attached:
    /// it is reached from config validation and from the launcher on
    /// machines that have never seen a real Amiga disk.
    #[test]
    fn enumeration_is_safe_with_nothing_attached() {
        let devices = list_devices().expect("IOKit enumeration works unprivileged");
        // Every host has at least the disk it booted from, and exactly one of
        // them is that disk.
        assert!(
            devices.iter().any(|d| d.safety == Safety::SystemDisk),
            "the running system's disk must be identified: {devices:#?}"
        );
        assert!(
            devices
                .iter()
                .filter(|d| d.safety == Safety::SystemDisk)
                .count()
                == 1,
            "exactly one device serves the root filesystem"
        );
        for device in &devices {
            assert!(!device.id.is_empty());
            assert!(device.size_bytes > 0);
            assert!(device.block_size >= 512);
            assert_eq!(device.path, PathBuf::from(format!("/dev/r{}", device.id)));
        }
    }
}
