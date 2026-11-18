// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Shared memory for the MIT-SHM extension: System V segments, and segments that a client passes
//! as a file descriptor (ShmAttachFd), with the `recvmsg` that receives such descriptors. This is
//! the only module with `unsafe` code.
use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileExt;

/// The most descriptors that one message may carry. More are closed and the read fails.
pub const MAX_FDS_PER_MESSAGE: usize = 16;

/// A shared memory segment that a client made, available to the server.
pub struct Segment {
    backing: Backing,
    len: usize,
}

enum Backing {
    /// A System V segment, mapped read-only.
    SystemV(*mut libc::c_void),
    /// A file (usually a memfd) that the client passed. It is read with `pread` and never mapped:
    /// a client that shrinks the file then gets a short read, and the server gets no SIGBUS.
    File(File),
}

impl Segment {
    /// Maps a segment for a client that runs as the user `client_uid`. Fails when the id does not
    /// exist, when the server may not read it (another IPC namespace), or when the client could
    /// not read it itself. The last check matters: the server may read more than the client may.
    pub fn attach(shm_id: i32, client_uid: u32) -> io::Result<Self> {
        // SAFETY: an all-zero `shmid_ds` is a valid value for `shmctl` to overwrite.
        let mut info: libc::shmid_ds = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid `shmid_ds` that lives for the call.
        if unsafe { libc::shmctl(shm_id, libc::IPC_STAT, &mut info) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let permission = &info.shm_perm;
        if !may_read(
            client_uid,
            permission.uid,
            permission.cuid,
            u32::from(permission.mode),
        ) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        // SAFETY: `shmat` takes no pointer that we own. A null address lets the system choose one.
        let address = unsafe { libc::shmat(shm_id, std::ptr::null(), libc::SHM_RDONLY) };
        if address as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            backing: Backing::SystemV(address),
            len: info.shm_segsz,
        })
    }

    /// A segment from a descriptor that the client passed. Only a regular file in memory is
    /// accepted: a memfd, or a file on tmpfs or hugetlbfs (`/dev/shm`). A pipe or a socket cannot
    /// be read at an offset, and a file on FUSE or NFS could block the one server thread in each
    /// read. The client could open the file itself, so reading it gives the server nothing that
    /// the client could not read.
    pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        if !in_memory(&fd)? {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let file = File::from(fd);
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let len = usize::try_from(meta.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        // A write-only or O_PATH descriptor fails here, and not later at each PutImage.
        file.read_at(&mut [0], 0)?;
        Ok(Self {
            backing: Backing::File(file),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Copies a part of the segment. `None` when the part reaches past the end.
    /// The client may write to the segment at the same time, so the bytes can be a mix of two
    /// frames. That is how X11 shared memory works, and the copy keeps the mix out of our memory.
    pub fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0; len];
        self.read_into(offset, &mut out)?;
        Some(out)
    }

    /// Like `read`, into a buffer of the caller. `None`, with `out` in any state, when the part
    /// reaches past the end.
    pub fn read_into(&self, offset: usize, out: &mut [u8]) -> Option<()> {
        let end = offset.checked_add(out.len())?;
        if end > self.len {
            return None;
        }
        match &self.backing {
            Backing::SystemV(address) => {
                let source = address.cast::<u8>().cast_const();
                // SAFETY: `offset..end` is inside the mapped segment, which stays mapped while
                // `self` lives, and `out` is our own memory of the same length, so the two do
                // not overlap. No reference to the segment is made: another process may write
                // to it during the copy. Every byte value is valid, so a mix of two frames is
                // possible and harmless. One copy for the block, as every X server does: a
                // volatile read for each byte took longer than drawing the image.
                unsafe {
                    std::ptr::copy_nonoverlapping(source.add(offset), out.as_mut_ptr(), out.len());
                }
                Some(())
            }
            Backing::File(file) => file.read_exact_at(out, offset as u64).ok(),
        }
    }
}

/// Whether the file of `fd` lives in memory (tmpfs, which holds memfds too, or hugetlbfs).
#[allow(clippy::unnecessary_cast)]
fn in_memory(fd: &OwnedFd) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `statfs` is a valid value for `fstatfs` to overwrite.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open for the call, and `info` is a valid `statfs` that lives for it.
    if unsafe { libc::fstatfs(fd.as_raw_fd(), &mut info) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // The types of these fields differ between C libraries, so all are cast to i64.
    let kind = info.f_type as i64;
    Ok(kind == libc::TMPFS_MAGIC as i64 || kind == libc::HUGETLBFS_MAGIC as i64)
}

/// Reads from a socket into `buf`, and takes the descriptors that come with the bytes
/// (SCM_RIGHTS). The descriptors are close-on-exec. A message with more than
/// `MAX_FDS_PER_MESSAGE` descriptors fails, and all of them are closed.
pub fn recv_with_fds(socket: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    const FD_SIZE: u32 = std::mem::size_of::<RawFd>() as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(FD_SIZE * MAX_FDS_PER_MESSAGE as u32) } as usize;
    // u64 elements keep the control buffer aligned for `cmsghdr`.
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // SAFETY: an all-zero `msghdr` is valid; the pointers are set below.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = (control.len() * 8) as _;
    // SAFETY: `message` points to `iov` (which points to `buf`) and to `control`, which all live
    // for the call; `recvmsg` writes only inside them.
    let read = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fds = Vec::new();
    // SAFETY: `message` was filled by `recvmsg`; CMSG_FIRSTHDR and CMSG_NXTHDR walk inside
    // `control`, and each SCM_RIGHTS payload holds whole descriptors that are now ours.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                // The kernel writes these lengths. They are checked anyway, so that a bad length
                // cannot make the loop read past `control`.
                let data = libc::CMSG_DATA(header).cast::<RawFd>();
                let control_end = control.as_ptr().cast::<u8>().add(control.len() * 8);
                let room = (control_end as usize).saturating_sub(data as usize);
                let payload = ((*header).cmsg_len as usize)
                    .saturating_sub(libc::CMSG_LEN(0) as usize)
                    .min(room);
                for index in 0..payload / FD_SIZE as usize {
                    let raw = data.add(index).read_unaligned();
                    fds.push(OwnedFd::from_raw_fd(raw));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        // Some descriptors did not fit; the ones that did are closed when `fds` drops.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many file descriptors in one message",
        ));
    }
    Ok((read as usize, fds))
}

/// The client side of a descriptor pass, for tests: one message with `bytes` and `fds`.
#[cfg(test)]
pub(super) fn send_with_fds(
    socket: &std::os::unix::net::UnixStream,
    bytes: &[u8],
    fds: &[std::os::fd::BorrowedFd],
) {
    use std::os::fd::AsRawFd;
    let fds_size = (std::mem::size_of::<RawFd>() * fds.len()) as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(fds_size) } as usize;
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: an all-zero `msghdr` is valid; the pointers are set below.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    if !fds.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = (control.len() * 8) as _;
    }
    // SAFETY: the header and its data lie inside `control`, which is big enough for `fds`;
    // `message` points only to buffers that live for the call.
    unsafe {
        if !fds.is_empty() {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(fds_size) as _;
            let data = libc::CMSG_DATA(header).cast::<RawFd>();
            for (index, fd) in fds.iter().enumerate() {
                data.add(index).write_unaligned(fd.as_raw_fd());
            }
        }
        assert_eq!(
            libc::sendmsg(socket.as_raw_fd(), &message, 0),
            bytes.len() as isize
        );
    }
}

/// The read rule of System V shared memory: the owner or the creator needs the owner bit,
/// anybody else needs the bit for others. Root may read everything.
fn may_read(client_uid: u32, owner: u32, creator: u32, mode: u32) -> bool {
    const OWNER_READ: u32 = 0o400;
    const OTHERS_READ: u32 = 0o004;
    if client_uid == 0 {
        return true;
    }
    if client_uid == owner || client_uid == creator {
        return mode & OWNER_READ != 0;
    }
    mode & OTHERS_READ != 0
}

impl Drop for Segment {
    fn drop(&mut self) {
        if let Backing::SystemV(address) = self.backing {
            // SAFETY: `address` came from `shmat` and is detached once, here.
            unsafe { libc::shmdt(address) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    fn temp_file(name: &str, bytes: &[u8]) -> (std::path::PathBuf, File) {
        let path = std::path::Path::new("/dev/shm")
            .join(format!("xsrv-shm-{name}-{}", std::process::id()));
        let mut file = File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        (path, file)
    }

    #[test]
    fn a_file_segment_reads_its_bytes_and_a_shrunk_file_gives_none() {
        let (path, file) = temp_file("read", &[1, 2, 3, 4, 5, 6, 7, 8]);
        let segment = Segment::from_fd(OwnedFd::from(file.try_clone().unwrap())).unwrap();
        assert_eq!(segment.len(), 8);
        assert_eq!(segment.read(2, 3), Some(vec![3, 4, 5]));
        assert_eq!(segment.read(6, 3), None, "past the end");
        file.set_len(4).unwrap();
        assert_eq!(segment.read(2, 3), None, "the client shrank the file");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn only_a_regular_file_can_be_a_segment() {
        let (socket, _) = UnixStream::pair().unwrap();
        assert!(Segment::from_fd(OwnedFd::from(socket)).is_err());
    }

    /// A file on a disk (here the crate's own manifest) could be on FUSE or NFS and block a read.
    #[test]
    fn a_file_outside_memory_is_refused() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let file = File::open(manifest).unwrap();
        assert!(!in_memory(&OwnedFd::from(file.try_clone().unwrap())).unwrap());
        assert!(Segment::from_fd(OwnedFd::from(file)).is_err());
    }

    #[test]
    fn a_write_only_descriptor_is_refused_at_once() {
        let (path, _) = temp_file("write-only", &[1, 2, 3, 4]);
        let write_only = File::options().write(true).open(&path).unwrap();
        assert!(Segment::from_fd(OwnedFd::from(write_only)).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn descriptors_arrive_with_the_bytes() {
        use std::os::fd::{AsFd, AsRawFd};
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (path, file) = temp_file("passed", &[9; 4]);
        send_with_fds(&sender, b"abcd", &[file.as_fd()]);
        let mut buf = [0u8; 16];
        let (read, fds) = recv_with_fds(receiver.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..read], b"abcd");
        assert_eq!(fds.len(), 1);
        let segment = Segment::from_fd(fds.into_iter().next().unwrap()).unwrap();
        assert_eq!(segment.read(0, 4), Some(vec![9; 4]));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_owner_needs_the_owner_bit_and_others_need_the_others_bit() {
        assert!(may_read(1000, 1000, 1000, 0o600));
        assert!(!may_read(1000, 1000, 1000, 0o200));
        assert!(!may_read(1001, 1000, 1000, 0o600));
        assert!(may_read(1001, 1000, 1000, 0o604));
        assert!(may_read(0, 1000, 1000, 0o000));
        assert!(may_read(1000, 5, 1000, 0o400));
    }
}
