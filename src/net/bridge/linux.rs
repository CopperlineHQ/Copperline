// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux AF_PACKET bridge and the FD-passing capability helper protocol.

use super::{AdapterIo, HostInterface};
use anyhow::{anyhow, bail, Context, Result};
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::mem::{size_of, zeroed};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ETH_P_ALL: u16 = 0x0003;
const HELPER_MAGIC: &[u8; 8] = b"CLNETFD1";
const MAX_INTERFACE_LEN: usize = libc::IFNAMSIZ - 1;

pub(super) fn list_interfaces() -> Result<Vec<HostInterface>> {
    let mut out = Vec::new();
    let interfaces = unsafe { libc::if_nameindex() };
    if interfaces.is_null() {
        return Err(std::io::Error::last_os_error()).context("if_nameindex");
    }
    let mut current = interfaces;
    while unsafe { (*current).if_index } != 0 {
        let name = unsafe { CStr::from_ptr((*current).if_name) }
            .to_string_lossy()
            .into_owned();
        let flags = interface_flags(&name).unwrap_or(0);
        let wireless = Path::new("/sys/class/net")
            .join(&name)
            .join("wireless")
            .exists();
        out.push(HostInterface {
            name,
            description: None,
            up: flags & libc::IFF_UP != 0,
            running: flags & libc::IFF_RUNNING != 0,
            loopback: flags & libc::IFF_LOOPBACK != 0,
            wireless,
        });
        current = unsafe { current.add(1) };
    }
    unsafe { libc::if_freenameindex(interfaces) };
    Ok(out)
}

fn interface_flags(name: &str) -> Result<i32> {
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if socket < 0 {
        return Err(std::io::Error::last_os_error()).context("opening ioctl socket");
    }
    let name = CString::new(name)?;
    let mut ifr: libc::ifreq = unsafe { zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            ifr.ifr_name.as_mut_ptr(),
            name.as_bytes_with_nul().len(),
        );
    }
    let rc = unsafe { libc::ioctl(socket, libc::SIOCGIFFLAGS, &mut ifr) };
    let error = std::io::Error::last_os_error();
    unsafe { libc::close(socket) };
    if rc < 0 {
        return Err(error).context("SIOCGIFFLAGS");
    }
    Ok(unsafe { ifr.ifr_ifru.ifru_flags } as i32)
}

pub(super) fn open(interface: &str, guest_mac: Option<[u8; 6]>) -> Result<LinuxAdapter> {
    match open_packet_socket(interface, guest_mac) {
        Ok(fd) => Ok(LinuxAdapter { fd }),
        Err(direct_error)
            if matches!(
                direct_error
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error),
                Some(libc::EPERM | libc::EACCES)
            ) =>
        {
            let fd = request_helper_fd(interface, guest_mac).with_context(|| {
                format!(
                    "direct AF_PACKET access was denied ({direct_error:#}); \
                     install/start copperline-net-helper"
                )
            })?;
            Ok(LinuxAdapter { fd })
        }
        Err(error) => Err(error),
    }
}

pub(super) struct LinuxAdapter {
    fd: OwnedFd,
}

impl AdapterIo for LinuxAdapter {
    fn send(&mut self, frame: &[u8]) -> Result<()> {
        let sent = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                frame.as_ptr().cast(),
                frame.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            return Err(std::io::Error::last_os_error()).context("AF_PACKET send");
        }
        if sent as usize != frame.len() {
            bail!("AF_PACKET short send ({sent} of {} bytes)", frame.len());
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>> {
        let mut frame = vec![0u8; 65_535];
        let received = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                frame.as_mut_ptr().cast(),
                frame.len(),
                0,
            )
        };
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error).context("AF_PACKET receive");
        }
        frame.truncate(received as usize);
        Ok(Some(frame))
    }

    fn set_guest_mac(&mut self, mac: [u8; 6]) -> Result<()> {
        attach_filter(self.fd.as_raw_fd(), Some(mac))
    }
}

use std::os::fd::AsRawFd;

fn attach_filter(fd: RawFd, guest_mac: Option<[u8; 6]>) -> Result<()> {
    // Kernel guard: admit the guest destination and Ethernet group addresses.
    // Source-MAC echo suppression is repeated in the worker.
    const BPF_LD: u16 = 0x00;
    const BPF_H: u16 = 0x08;
    const BPF_W: u16 = 0x00;
    const BPF_B: u16 = 0x10;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_JSET: u16 = 0x40;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;

    let instruction = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let mut instructions = if let Some(mac) = guest_mac {
        let first = u32::from(u16::from_be_bytes([mac[0], mac[1]]));
        let rest = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
        vec![
            instruction(BPF_LD | BPF_H | BPF_ABS, 0, 0, 0),
            instruction(BPF_JMP | BPF_JEQ | BPF_K, 0, 2, first),
            instruction(BPF_LD | BPF_W | BPF_ABS, 0, 0, 2),
            instruction(BPF_JMP | BPF_JEQ | BPF_K, 2, 0, rest),
            instruction(BPF_LD | BPF_B | BPF_ABS, 0, 0, 0),
            instruction(BPF_JMP | BPF_JSET | BPF_K, 0, 1, 1),
            instruction(BPF_RET | BPF_K, 0, 0, 65_535),
            instruction(BPF_RET | BPF_K, 0, 0, 0),
        ]
    } else {
        // Before a generic plugin NIC transmits its first frame (and reveals
        // its source MAC), multicast/broadcast is the only useful ingress.
        vec![
            instruction(BPF_LD | BPF_B | BPF_ABS, 0, 0, 0),
            instruction(BPF_JMP | BPF_JSET | BPF_K, 0, 1, 1),
            instruction(BPF_RET | BPF_K, 0, 0, 65_535),
            instruction(BPF_RET | BPF_K, 0, 0, 0),
        ]
    };
    let program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ATTACH_FILTER,
            (&program as *const libc::sock_fprog).cast(),
            size_of::<libc::sock_fprog>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).context("attaching AF_PACKET receive filter");
    }
    Ok(())
}

fn open_packet_socket(interface: &str, guest_mac: Option<[u8; 6]>) -> Result<OwnedFd> {
    if interface.len() > MAX_INTERFACE_LEN {
        bail!("interface name is too long: {interface:?}");
    }
    let name = CString::new(interface).context("interface name contains a NUL")?;
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("network interface {interface:?} was not found"));
    }
    let raw = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            i32::from(ETH_P_ALL.to_be()),
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context("opening AF_PACKET socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    let address = libc::sockaddr_ll {
        sll_family: libc::AF_PACKET as u16,
        sll_protocol: ETH_P_ALL.to_be(),
        sll_ifindex: index as i32,
        sll_hatype: 0,
        sll_pkttype: 0,
        sll_halen: 0,
        sll_addr: [0; 8],
    };
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_ll).cast(),
            size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("binding AF_PACKET socket to {interface:?}"));
    }

    let membership = libc::packet_mreq {
        mr_ifindex: index as i32,
        mr_type: libc::PACKET_MR_PROMISC as u16,
        mr_alen: 0,
        mr_address: [0; 8],
    };
    let rc = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_PACKET,
            libc::PACKET_ADD_MEMBERSHIP,
            (&membership as *const libc::packet_mreq).cast(),
            size_of::<libc::packet_mreq>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("enabling promiscuous capture on {interface:?}"));
    }
    attach_filter(fd.as_raw_fd(), guest_mac)?;
    Ok(fd)
}

/// Per-user helper control socket.
pub fn helper_socket_path() -> Result<PathBuf> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(runtime)
            .join("copperline-net-helper")
            .join("control.sock"));
    }
    Ok(PathBuf::from(format!(
        "/run/user/{}/copperline-net-helper/control.sock",
        unsafe { libc::geteuid() }
    )))
}

fn request_helper_fd(interface: &str, guest_mac: Option<[u8; 6]>) -> Result<OwnedFd> {
    if interface.len() > MAX_INTERFACE_LEN {
        bail!("interface name is too long: {interface:?}");
    }
    let path = helper_socket_path()?;
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(first_error) => {
            start_helper().with_context(|| {
                format!(
                    "connecting to helper socket {} failed: {first_error}",
                    path.display()
                )
            })?;
            let mut connected = None;
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(25));
                if let Ok(stream) = UnixStream::connect(&path) {
                    connected = Some(stream);
                    break;
                }
            }
            connected.ok_or_else(|| {
                anyhow!(
                    "helper did not create its control socket at {}",
                    path.display()
                )
            })?
        }
    };
    stream.write_all(HELPER_MAGIC)?;
    stream.write_all(&(interface.len() as u16).to_be_bytes())?;
    stream.write_all(interface.as_bytes())?;
    stream.write_all(&guest_mac.unwrap_or([0; 6]))?;
    receive_fd(&stream)
}

fn start_helper() -> Result<()> {
    let current = std::env::current_exe().context("locating Copperline executable")?;
    // Prefer the systemd user socket installed by the companion setup. It is
    // also the path Flatpak uses: connecting to the exported runtime socket
    // activates this host service without granting the sandbox raw devices.
    let systemd_started = std::process::Command::new("systemctl")
        .args(["--user", "start", "copperline-net-helper.socket"])
        .status()
        .is_ok_and(|status| status.success());
    if systemd_started {
        return Ok(());
    }
    let sibling = current.with_file_name("copperline-net-helper");
    let system = PathBuf::from("/usr/libexec/copperline/copperline-net-helper");
    let helper = if system.exists() { &system } else { &sibling };
    std::process::Command::new(helper)
        .arg("--serve")
        .spawn()
        .with_context(|| {
            format!(
                "starting {}; run `copperline --install-net-helper` first",
                helper.display()
            )
        })?;
    Ok(())
}

fn receive_fd(stream: &UnixStream) -> Result<OwnedFd> {
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&mut byte as *mut u8).cast(),
        iov_len: 1,
    };
    let mut control = [0u8; 64];
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, 0) };
    if received < 0 {
        return Err(std::io::Error::last_os_error()).context("receiving helper response");
    }
    if byte != 0 {
        let mut text = String::new();
        (&mut &*stream).read_to_string(&mut text).ok();
        bail!("network helper rejected request: {}", text.trim());
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null()
        || unsafe { (*header).cmsg_level } != libc::SOL_SOCKET
        || unsafe { (*header).cmsg_type } != libc::SCM_RIGHTS
    {
        bail!("network helper response carried no AF_PACKET file descriptor");
    }
    let raw = unsafe { *libc::CMSG_DATA(header).cast::<RawFd>() };
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Run the narrowly scoped per-user capability helper. The executable should
/// be root-owned with only `cap_net_raw=ep`; it never handles Ethernet frames,
/// and passes one configured AF_PACKET descriptor per validated request.
pub fn run_helper() -> Result<()> {
    ensure_helper_group()?;
    if systemd_listener_available() {
        let listener = unsafe { UnixListener::from_raw_fd(3) };
        return serve_connections(listener);
    }
    let path = helper_socket_path()?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("helper socket has no parent directory"))?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => bail!("network helper is already running"),
            Err(_) => std::fs::remove_file(&path)
                .with_context(|| format!("removing stale {}", path.display()))?,
        }
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    serve_connections(listener)
}

fn serve_connections(listener: UnixListener) -> Result<()> {
    for connection in listener.incoming() {
        let mut stream = connection?;
        if let Err(error) = serve_request(&mut stream) {
            let _ = stream.write_all(&[1]);
            let _ = writeln!(stream, "{error:#}");
        }
    }
    Ok(())
}

fn systemd_listener_available() -> bool {
    std::env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id())
        && std::env::var("LISTEN_FDS").ok().as_deref() == Some("1")
}

fn ensure_helper_group() -> Result<()> {
    let name = CString::new("copperline-net").unwrap();
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    if group.is_null() {
        bail!(
            "the copperline-net group is missing; run \
             `copperline --install-net-helper`"
        );
    }
    let target = unsafe { (*group).gr_gid };
    if unsafe { libc::getegid() } == target {
        return Ok(());
    }
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(std::io::Error::last_os_error()).context("reading supplementary groups");
    }
    let mut groups = vec![0 as libc::gid_t; count as usize];
    if unsafe { libc::getgroups(count, groups.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error()).context("reading supplementary groups");
    }
    if !groups.contains(&target) {
        bail!(
            "this user is not in the copperline-net group; log out and back in \
             after installing the helper"
        );
    }
    Ok(())
}

fn serve_request(stream: &mut UnixStream) -> Result<()> {
    let mut magic = [0u8; HELPER_MAGIC.len()];
    stream.read_exact(&mut magic)?;
    if &magic != HELPER_MAGIC {
        bail!("unsupported helper protocol");
    }
    let mut length = [0u8; 2];
    stream.read_exact(&mut length)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length == 0 || length > MAX_INTERFACE_LEN {
        bail!("invalid interface name length");
    }
    let mut name = vec![0u8; length];
    stream.read_exact(&mut name)?;
    let name = std::str::from_utf8(&name).context("interface name is not UTF-8")?;
    let mut mac = [0u8; 6];
    stream.read_exact(&mut mac)?;
    let mac = (mac != [0; 6]).then_some(mac);
    let fd = open_packet_socket(name, mac)?;
    send_fd(stream, fd.as_raw_fd())
}

fn send_fd(stream: &UnixStream, fd: RawFd) -> Result<()> {
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&mut byte as *mut u8).cast(),
        iov_len: 1,
    };
    let space = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as usize;
        *libc::CMSG_DATA(header).cast::<RawFd>() = fd;
    }
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error()).context("sending AF_PACKET descriptor");
    }
    Ok(())
}
