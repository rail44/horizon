//! Exact-endpoint network decisions for the combined seccomp listener.
//!
//! The notification shape is derived from nono-cli v0.68.0
//! `exec_strategy/supervisor_linux.rs`. Horizon deliberately does not copy
//! upstream's `is_loopback && port` + CONTINUE decision: it would permit a
//! same-port decoy on another loopback address and has a documented pointer
//! race. An allowed connect is performed on a duplicated child socket using
//! the trusted fixed endpoint, then the original syscall is completed without
//! dereferencing the child's pointer again.

use nono::sandbox::{
    continue_notif, deny_notif, notif_id_valid, read_mmsghdr_dests, read_msghdr_dest,
    respond_notif_errno, SeccompNotif, SYS_BIND, SYS_CONNECT, SYS_SENDMMSG, SYS_SENDMSG,
    SYS_SENDTO,
};
use std::io::{Read, Seek};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

const MAX_IPC_DENIALS: usize = 1_000;
const SOCK_TYPE_MASK: u64 = 0x0f;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkEnforcement {
    Blocked,
    ProxyOnly(SocketAddr),
}

#[derive(Debug)]
enum AttemptedAddress {
    Inet(SocketAddr),
    UnixPath(PathBuf),
    UnixAbstract,
    UnixUnnamed,
    Other(u16),
}

pub(crate) fn handle_network_notification(
    notify_fd: RawFd,
    notification: SeccompNotif,
    enforcement: NetworkEnforcement,
    denials: &mut Vec<nono::IpcDenialRecord>,
) -> nono::Result<()> {
    let syscall = notification.data.nr;
    match syscall {
        syscall if syscall == libc::SYS_socket as i32 => {
            let family = notification.data.args[0] as libc::c_int;
            let socket_type = (notification.data.args[1] & SOCK_TYPE_MASK) as libc::c_int;
            let allowed_family = matches!(family, libc::AF_INET | libc::AF_INET6 | libc::AF_UNIX);
            let allowed_type = socket_type == libc::SOCK_STREAM
                || (family == libc::AF_UNIX && socket_type == libc::SOCK_SEQPACKET);
            if allowed_family && allowed_type {
                continue_if_live(notify_fd, notification.id)
            } else {
                record_raw_denial(
                    denials,
                    format!("socket-family:{family}/type:{socket_type}"),
                    "socket",
                    "only TCP streams and local stream/seqpacket IPC are allowed".to_string(),
                );
                respond_notif_errno(notify_fd, notification.id, libc::EACCES)
            }
        }
        syscall if syscall == libc::SYS_socketpair as i32 => {
            let family = notification.data.args[0] as libc::c_int;
            let socket_type = (notification.data.args[1] & SOCK_TYPE_MASK) as libc::c_int;
            if family == libc::AF_UNIX
                && matches!(socket_type, libc::SOCK_STREAM | libc::SOCK_SEQPACKET)
            {
                continue_if_live(notify_fd, notification.id)
            } else {
                record_raw_denial(
                    denials,
                    format!("socketpair-family:{family}/type:{socket_type}"),
                    "socketpair",
                    "only local stream/seqpacket socket pairs are allowed".to_string(),
                );
                respond_notif_errno(notify_fd, notification.id, libc::EACCES)
            }
        }
        syscall if syscall == libc::SYS_io_uring_setup as i32 => {
            record_raw_denial(
                denials,
                "io_uring".to_string(),
                "io_uring_setup",
                "io_uring is disabled because it can bypass syscall mediation".to_string(),
            );
            respond_notif_errno(notify_fd, notification.id, libc::EACCES)
        }
        SYS_CONNECT => {
            let attempted = read_address(
                notification.pid,
                notification.data.args[1],
                notification.data.args[2],
            )?;
            if !notif_id_valid(notify_fd, notification.id)? {
                return Ok(());
            }
            if let (NetworkEnforcement::ProxyOnly(proxy), AttemptedAddress::Inet(attempted)) =
                (enforcement, &attempted)
            {
                if *attempted == proxy {
                    return connect_trusted_endpoint(notify_fd, &notification, proxy, denials);
                }
            }
            record_denial(denials, &attempted, "connect");
            respond_notif_errno(notify_fd, notification.id, libc::EACCES)
        }
        SYS_BIND => {
            let attempted = read_address(
                notification.pid,
                notification.data.args[1],
                notification.data.args[2],
            )?;
            if !notif_id_valid(notify_fd, notification.id)? {
                return Ok(());
            }
            record_denial(denials, &attempted, "bind");
            respond_notif_errno(notify_fd, notification.id, libc::EACCES)
        }
        SYS_SENDTO => {
            if notification.data.args[4] == 0 {
                return continue_if_live(notify_fd, notification.id);
            }
            let attempted = read_address(
                notification.pid,
                notification.data.args[4],
                notification.data.args[5],
            )?;
            deny_destination(notify_fd, &notification, attempted, "sendto", denials)
        }
        SYS_SENDMSG => match read_msghdr_dest(notification.pid, notification.data.args[1])? {
            None => continue_if_live(notify_fd, notification.id),
            Some((pointer, length)) => {
                let attempted = read_address(notification.pid, pointer, length)?;
                deny_destination(notify_fd, &notification, attempted, "sendmsg", denials)
            }
        },
        SYS_SENDMMSG => {
            let destinations = read_mmsghdr_dests(
                notification.pid,
                notification.data.args[1],
                notification.data.args[2],
            )?;
            let Some((pointer, length)) = destinations.into_iter().flatten().next() else {
                return continue_if_live(notify_fd, notification.id);
            };
            let attempted = read_address(notification.pid, pointer, length)?;
            deny_destination(notify_fd, &notification, attempted, "sendmmsg", denials)
        }
        _ => deny_notif(notify_fd, notification.id),
    }
}

fn connect_trusted_endpoint(
    notify_fd: RawFd,
    notification: &SeccompNotif,
    proxy: SocketAddr,
    denials: &mut Vec<nono::IpcDenialRecord>,
) -> nono::Result<()> {
    let socket_fd = match duplicate_child_fd(notification.pid, notification.data.args[0]) {
        Ok(fd) => fd,
        Err(error) => {
            record_raw_denial(
                denials,
                proxy.to_string(),
                "connect",
                format!("could not inspect the child socket: {error}"),
            );
            return respond_notif_errno(notify_fd, notification.id, libc::EACCES);
        }
    };
    if socket_property(socket_fd.as_raw_fd(), libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_property(socket_fd.as_raw_fd(), libc::SO_DOMAIN)? != libc::AF_INET
    {
        record_raw_denial(
            denials,
            proxy.to_string(),
            "connect",
            "only an IPv4 TCP stream may connect to the session proxy".to_string(),
        );
        return respond_notif_errno(notify_fd, notification.id, libc::EACCES);
    }
    if !notif_id_valid(notify_fd, notification.id)? {
        return Ok(());
    }

    let SocketAddr::V4(proxy) = proxy else {
        return respond_notif_errno(notify_fd, notification.id, libc::EACCES);
    };
    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: proxy.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(proxy.ip().octets()),
        },
        sin_zero: [0; 8],
    };
    let result = unsafe {
        libc::connect(
            socket_fd.as_raw_fd(),
            (&address as *const libc::sockaddr_in).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if result == 0 {
        respond_notif_errno(notify_fd, notification.id, 0)
    } else {
        let errno = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        respond_notif_errno(notify_fd, notification.id, errno)
    }
}

fn deny_destination(
    notify_fd: RawFd,
    notification: &SeccompNotif,
    attempted: AttemptedAddress,
    operation: &str,
    denials: &mut Vec<nono::IpcDenialRecord>,
) -> nono::Result<()> {
    if !notif_id_valid(notify_fd, notification.id)? {
        return Ok(());
    }
    record_denial(denials, &attempted, operation);
    respond_notif_errno(notify_fd, notification.id, libc::EACCES)
}

fn continue_if_live(notify_fd: RawFd, notification_id: u64) -> nono::Result<()> {
    if notif_id_valid(notify_fd, notification_id)? {
        continue_notif(notify_fd, notification_id)
    } else {
        Ok(())
    }
}

fn duplicate_child_fd(pid: u32, raw_fd: u64) -> nono::Result<OwnedFd> {
    let child_fd = i32::try_from(raw_fd).map_err(|_| {
        nono::NonoError::SandboxInit(format!("child socket fd is out of range: {raw_fd}"))
    })?;
    let pidfd_raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) };
    if pidfd_raw < 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "pidfd_open failed for child {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd_raw as RawFd) };
    let duplicated =
        unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), child_fd, 0_u32) };
    if duplicated < 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "pidfd_getfd failed for child {pid} fd {child_fd}: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated as RawFd) })
}

fn socket_property(fd: RawFd, property: libc::c_int) -> nono::Result<libc::c_int> {
    let mut value = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            property,
            (&mut value as *mut libc::c_int).cast::<libc::c_void>(),
            &mut length,
        )
    };
    if result == 0 && length as usize == std::mem::size_of::<libc::c_int>() {
        Ok(value)
    } else {
        Err(nono::NonoError::SandboxInit(format!(
            "getsockopt({property}) failed: {}",
            std::io::Error::last_os_error()
        )))
    }
}

fn read_address(pid: u32, pointer: u64, length: u64) -> nono::Result<AttemptedAddress> {
    if length < 2 {
        return Err(nono::NonoError::SandboxInit(
            "sockaddr is too short".to_string(),
        ));
    }
    let requested = usize::try_from(length).map_err(|_| {
        nono::NonoError::SandboxInit(format!("sockaddr length is too large: {length}"))
    })?;
    let maximum = std::mem::size_of::<libc::sockaddr_un>().max(28);
    if requested > std::mem::size_of::<libc::sockaddr_un>() {
        return Err(nono::NonoError::SandboxInit(format!(
            "sockaddr length exceeds the supported maximum: {requested}"
        )));
    }
    let mut bytes = vec![0_u8; requested.min(maximum)];
    let path = format!("/proc/{pid}/mem");
    let mut memory = std::fs::File::open(&path)
        .map_err(|error| nono::NonoError::SandboxInit(format!("failed to open {path}: {error}")))?;
    memory
        .seek(std::io::SeekFrom::Start(pointer))
        .map_err(|error| nono::NonoError::SandboxInit(format!("failed to seek {path}: {error}")))?;
    memory.read_exact(&mut bytes).map_err(|error| {
        nono::NonoError::SandboxInit(format!("failed to read sockaddr from {path}: {error}"))
    })?;
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    match family as libc::c_int {
        libc::AF_INET if bytes.len() >= 8 => {
            let port = u16::from_be_bytes([bytes[2], bytes[3]]);
            let ip = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
            Ok(AttemptedAddress::Inet(SocketAddr::V4(SocketAddrV4::new(
                ip, port,
            ))))
        }
        libc::AF_INET6 if bytes.len() >= 24 => {
            let port = u16::from_be_bytes([bytes[2], bytes[3]]);
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(&bytes[8..24]);
            Ok(AttemptedAddress::Inet(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(octets),
                port,
                0,
                0,
            ))))
        }
        libc::AF_UNIX if bytes.len() == 2 => Ok(AttemptedAddress::UnixUnnamed),
        libc::AF_UNIX if bytes.get(2) == Some(&0) => Ok(AttemptedAddress::UnixAbstract),
        libc::AF_UNIX => {
            let path = &bytes[2..];
            let end = path
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(path.len());
            Ok(AttemptedAddress::UnixPath(
                std::ffi::OsString::from_vec(path[..end].to_vec()).into(),
            ))
        }
        _ => Ok(AttemptedAddress::Other(family)),
    }
}

fn record_denial(
    denials: &mut Vec<nono::IpcDenialRecord>,
    attempted: &AttemptedAddress,
    operation: &str,
) {
    let target = match attempted {
        AttemptedAddress::Inet(address) => address.to_string(),
        AttemptedAddress::UnixPath(path) => path.display().to_string(),
        AttemptedAddress::UnixAbstract => "unix:<abstract>".to_string(),
        AttemptedAddress::UnixUnnamed => "unix:<unnamed>".to_string(),
        AttemptedAddress::Other(family) => format!("socket-family:{family}"),
    };
    record_raw_denial(
        denials,
        target,
        operation,
        "the route is outside the fixed session proxy boundary".to_string(),
    );
}

fn record_raw_denial(
    denials: &mut Vec<nono::IpcDenialRecord>,
    target: String,
    operation: &str,
    reason: String,
) {
    if denials.len() < MAX_IPC_DENIALS {
        denials.push(nono::IpcDenialRecord::new(
            target,
            operation.to_string(),
            reason,
            None,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_proxy_address_is_not_equivalent_to_the_rest_of_loopback() {
        let proxy: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let decoy: SocketAddr = "127.0.0.2:8080".parse().unwrap();
        assert_ne!(proxy, decoy);
    }
}
