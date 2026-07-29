// SPDX-License-Identifier: GPL-3.0-or-later

//! Runtime-loaded Npcap bridge.
//!
//! Npcap's SDK DLL is not redistributable with Copperline's normal release.
//! Keeping every entry point behind `libloading` means NAT-only users have no
//! `wpcap.dll` process dependency. Only absolute paths beneath the Windows
//! system directory are considered.

use super::{mac_filter, AdapterIo, HostInterface};
use anyhow::{anyhow, bail, Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_uchar, c_uint, c_void, CStr, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

const PCAP_ERRBUF_SIZE: usize = 256;
const DLT_EN10MB: c_int = 1;
const PCAP_IF_LOOPBACK: c_uint = 0x1;
const PCAP_IF_UP: c_uint = 0x2;
const PCAP_IF_RUNNING: c_uint = 0x4;
const PCAP_IF_WIRELESS: c_uint = 0x8;

type Pcap = c_void;

#[repr(C)]
struct PcapIf {
    next: *mut PcapIf,
    name: *mut c_char,
    description: *mut c_char,
    addresses: *mut c_void,
    flags: c_uint,
}

#[repr(C)]
struct Timeval {
    tv_sec: i32,
    tv_usec: i32,
}

#[repr(C)]
struct PcapPacketHeader {
    ts: Timeval,
    caplen: c_uint,
    len: c_uint,
}

#[repr(C)]
struct BpfInsn {
    code: u16,
    jt: u8,
    jf: u8,
    k: c_uint,
}

#[repr(C)]
struct BpfProgram {
    bf_len: c_uint,
    bf_insns: *mut BpfInsn,
}

type FindAllDevs = unsafe extern "C" fn(*mut *mut PcapIf, *mut c_char) -> c_int;
type FreeAllDevs = unsafe extern "C" fn(*mut PcapIf);
type Create = unsafe extern "C" fn(*const c_char, *mut c_char) -> *mut Pcap;
type SetInt = unsafe extern "C" fn(*mut Pcap, c_int) -> c_int;
type Activate = unsafe extern "C" fn(*mut Pcap) -> c_int;
type SetNonblock = unsafe extern "C" fn(*mut Pcap, c_int, *mut c_char) -> c_int;
type Datalink = unsafe extern "C" fn(*mut Pcap) -> c_int;
type Compile =
    unsafe extern "C" fn(*mut Pcap, *mut BpfProgram, *const c_char, c_int, c_uint) -> c_int;
type SetFilter = unsafe extern "C" fn(*mut Pcap, *mut BpfProgram) -> c_int;
type FreeCode = unsafe extern "C" fn(*mut BpfProgram);
type NextEx =
    unsafe extern "C" fn(*mut Pcap, *mut *const PcapPacketHeader, *mut *const c_uchar) -> c_int;
type SendPacket = unsafe extern "C" fn(*mut Pcap, *const c_uchar, c_int) -> c_int;
type GetErr = unsafe extern "C" fn(*mut Pcap) -> *const c_char;
type Close = unsafe extern "C" fn(*mut Pcap);

struct Api {
    _library: Library,
    findalldevs: FindAllDevs,
    freealldevs: FreeAllDevs,
    create: Create,
    set_snaplen: SetInt,
    set_promisc: SetInt,
    set_timeout: SetInt,
    set_immediate_mode: Option<SetInt>,
    activate: Activate,
    setnonblock: SetNonblock,
    datalink: Datalink,
    compile: Compile,
    setfilter: SetFilter,
    freecode: FreeCode,
    next_ex: NextEx,
    sendpacket: SendPacket,
    geterr: GetErr,
    close: Close,
}

impl Api {
    fn load() -> Result<Arc<Self>> {
        let system = windows_system_directory()?;
        let candidates = [
            system.join("Npcap").join("wpcap.dll"),
            system.join("wpcap.dll"),
        ];
        let mut failures = Vec::new();
        for path in candidates {
            match unsafe { Self::load_from(&path) } {
                Ok(api) => return Ok(Arc::new(api)),
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        bail!(
            "Npcap is required for bridged networking but wpcap.dll could not \
             be loaded from the Windows system directory. Install Npcap from \
             https://npcap.com/ (WinPcap API-compatible mode is supported). {}",
            failures.join("; ")
        )
    }

    unsafe fn load_from(path: &std::path::Path) -> Result<Self> {
        let library =
            unsafe { Library::new(path) }.with_context(|| format!("loading {}", path.display()))?;
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {
                *unsafe { library.get::<$ty>(concat!($name, "\0").as_bytes()) }
                    .with_context(|| format!("Npcap lacks {}", $name))?
            };
        }
        let set_immediate_mode = unsafe {
            library
                .get::<SetInt>(b"pcap_set_immediate_mode\0")
                .ok()
                .map(|symbol| *symbol)
        };
        Ok(Self {
            findalldevs: symbol!("pcap_findalldevs", FindAllDevs),
            freealldevs: symbol!("pcap_freealldevs", FreeAllDevs),
            create: symbol!("pcap_create", Create),
            set_snaplen: symbol!("pcap_set_snaplen", SetInt),
            set_promisc: symbol!("pcap_set_promisc", SetInt),
            set_timeout: symbol!("pcap_set_timeout", SetInt),
            set_immediate_mode,
            activate: symbol!("pcap_activate", Activate),
            setnonblock: symbol!("pcap_setnonblock", SetNonblock),
            datalink: symbol!("pcap_datalink", Datalink),
            compile: symbol!("pcap_compile", Compile),
            setfilter: symbol!("pcap_setfilter", SetFilter),
            freecode: symbol!("pcap_freecode", FreeCode),
            next_ex: symbol!("pcap_next_ex", NextEx),
            sendpacket: symbol!("pcap_sendpacket", SendPacket),
            geterr: symbol!("pcap_geterr", GetErr),
            close: symbol!("pcap_close", Close),
            _library: library,
        })
    }
}

fn windows_system_directory() -> Result<PathBuf> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32_768];
    let length =
        unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len().try_into().unwrap()) };
    if length == 0 || length as usize >= buffer.len() {
        bail!("GetSystemDirectoryW failed");
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(String::from_utf16(&buffer)?))
}

pub(super) fn list_interfaces() -> Result<Vec<HostInterface>> {
    let api = Api::load()?;
    let mut first = ptr::null_mut();
    let mut error = [0 as c_char; PCAP_ERRBUF_SIZE];
    let rc = unsafe { (api.findalldevs)(&mut first, error.as_mut_ptr()) };
    if rc != 0 {
        bail!("Npcap interface enumeration failed: {}", error_text(&error));
    }
    struct Guard<'a>(&'a Api, *mut PcapIf);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            unsafe { (self.0.freealldevs)(self.1) };
        }
    }
    let _guard = Guard(&api, first);
    let mut out = Vec::new();
    let mut current = first;
    while !current.is_null() {
        let item = unsafe { &*current };
        if !item.name.is_null() {
            let name = unsafe { CStr::from_ptr(item.name) }
                .to_string_lossy()
                .into_owned();
            let description = (!item.description.is_null()).then(|| {
                unsafe { CStr::from_ptr(item.description) }
                    .to_string_lossy()
                    .into_owned()
            });
            out.push(HostInterface {
                name,
                description,
                up: item.flags & PCAP_IF_UP != 0,
                running: item.flags & PCAP_IF_RUNNING != 0,
                loopback: item.flags & PCAP_IF_LOOPBACK != 0,
                wireless: item.flags & PCAP_IF_WIRELESS != 0,
            });
        }
        current = item.next;
    }
    Ok(out)
}

pub(super) fn open(interface: &str, _guest_mac: Option<[u8; 6]>) -> Result<WindowsAdapter> {
    let api = Api::load()?;
    let name = CString::new(interface).context("interface name contains a NUL")?;
    let mut error = [0 as c_char; PCAP_ERRBUF_SIZE];
    let handle = unsafe { (api.create)(name.as_ptr(), error.as_mut_ptr()) };
    if handle.is_null() {
        bail!(
            "Npcap could not create adapter {interface:?}: {}",
            error_text(&error)
        );
    }
    let mut adapter = WindowsAdapter { api, handle };
    adapter.configure(interface)?;
    Ok(adapter)
}

pub(super) struct WindowsAdapter {
    api: Arc<Api>,
    handle: *mut Pcap,
}

// The handle is owned and used only by the bridge worker after construction.
unsafe impl Send for WindowsAdapter {}

impl WindowsAdapter {
    fn configure(&mut self, interface: &str) -> Result<()> {
        for (name, setter, value) in [
            ("snapshot length", self.api.set_snaplen, 65_535),
            ("promiscuous mode", self.api.set_promisc, 1),
            ("read timeout", self.api.set_timeout, 1),
        ] {
            if unsafe { setter(self.handle, value) } != 0 {
                bail!("Npcap rejected {name} for {interface:?}: {}", self.error());
            }
        }
        if let Some(set_immediate) = self.api.set_immediate_mode {
            if unsafe { set_immediate(self.handle, 1) } != 0 {
                bail!(
                    "Npcap rejected immediate mode for {interface:?}: {}",
                    self.error()
                );
            }
        }
        if unsafe { (self.api.activate)(self.handle) } < 0 {
            bail!("Npcap could not activate {interface:?}: {}", self.error());
        }
        if unsafe { (self.api.datalink)(self.handle) } != DLT_EN10MB {
            bail!("Npcap adapter {interface:?} does not expose Ethernet frames");
        }
        let mut error = [0 as c_char; PCAP_ERRBUF_SIZE];
        if unsafe { (self.api.setnonblock)(self.handle, 1, error.as_mut_ptr()) } != 0 {
            bail!(
                "Npcap could not make {interface:?} non-blocking: {}",
                error_text(&error)
            );
        }
        Ok(())
    }

    fn error(&self) -> String {
        let text = unsafe { (self.api.geterr)(self.handle) };
        if text.is_null() {
            return "unknown error".to_string();
        }
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

impl AdapterIo for WindowsAdapter {
    fn send(&mut self, frame: &[u8]) -> Result<()> {
        let length: c_int = frame
            .len()
            .try_into()
            .map_err(|_| anyhow!("Ethernet frame is too large"))?;
        if unsafe { (self.api.sendpacket)(self.handle, frame.as_ptr(), length) } != 0 {
            bail!("Npcap send failed: {}", self.error());
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>> {
        let mut header = ptr::null();
        let mut bytes = ptr::null();
        match unsafe { (self.api.next_ex)(self.handle, &mut header, &mut bytes) } {
            1 => {
                if header.is_null() || bytes.is_null() {
                    bail!("Npcap returned an empty packet");
                }
                let length = unsafe { (*header).caplen as usize };
                Ok(Some(unsafe {
                    std::slice::from_raw_parts(bytes, length).to_vec()
                }))
            }
            0 => Ok(None),
            -1 => bail!("Npcap receive failed: {}", self.error()),
            -2 => bail!("Npcap capture ended"),
            result => bail!("Npcap returned unexpected receive status {result}"),
        }
    }

    fn set_guest_mac(&mut self, mac: [u8; 6]) -> Result<()> {
        let filter = CString::new(mac_filter(mac)).unwrap();
        let mut program = BpfProgram {
            bf_len: 0,
            bf_insns: ptr::null_mut(),
        };
        if unsafe { (self.api.compile)(self.handle, &mut program, filter.as_ptr(), 1, 0xffff_ffff) }
            != 0
        {
            bail!("Npcap could not compile receive filter: {}", self.error());
        }
        let result = unsafe { (self.api.setfilter)(self.handle, &mut program) };
        unsafe { (self.api.freecode)(&mut program) };
        if result != 0 {
            bail!("Npcap could not install receive filter: {}", self.error());
        }
        Ok(())
    }
}

impl Drop for WindowsAdapter {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.api.close)(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

fn error_text(buffer: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
