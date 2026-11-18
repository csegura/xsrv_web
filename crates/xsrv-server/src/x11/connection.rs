// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, Interest};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::Notify;

use super::codec::Endian;
use super::extensions::MAX_BIG_REQUEST_BYTES;
use super::setup::{failure_reply, success_reply, SetupRequest, PROTOCOL_MAJOR, SETUP_HEADER_LEN};
use super::shm::recv_with_fds;
use super::state::{ClientId, PendingBytes, State, MAX_QUEUED_FDS};

const REQUEST_HEADER_LEN: usize = 4;
const YIELD_EVERY: u32 = 32;
/// How much one read takes from the socket.
const READ_CHUNK: usize = 64 * 1024;
/// The time that a new connection has to send its setup request.
const SETUP_TIME_LIMIT: Duration = Duration::from_secs(10);
/// Connections that may wait for their setup at the same time.
pub const MAX_PENDING_SETUPS: usize = 32;

pub type SharedState = Rc<RefCell<State>>;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A place among the connections that have not finished the setup. A local user could otherwise
/// open sockets that never send the setup and keep them, one open descriptor each.
pub struct SetupSlot(Rc<Cell<usize>>);

impl SetupSlot {
    pub fn take(pending: &Rc<Cell<usize>>) -> Option<Self> {
        if pending.get() >= MAX_PENDING_SETUPS {
            return None;
        }
        pending.set(pending.get() + 1);
        Some(Self(Rc::clone(pending)))
    }
}

impl Drop for SetupSlot {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

pub async fn run_client(stream: UnixStream, state: SharedState, slot: SetupSlot) -> io::Result<()> {
    let peer_uid = stream.peer_cred().ok().map(|cred| cred.uid());
    let (reader, mut writer) = stream.into_split();
    let mut reader = ClientReader::new(reader);
    let (setup, trailer) = read_setup(&mut reader, SETUP_TIME_LIMIT).await?;
    drop(slot);

    let (tx, rx) = unbounded_channel();
    let pending: PendingBytes = Rc::new(Cell::new(0));
    let overflow = Rc::new(Notify::new());
    let big_requests = Rc::new(Cell::new(false));
    let cookie = state.borrow().x11_cookie().map(<[u8]>::to_vec);
    let allowed = cookie
        .as_deref()
        .is_none_or(|expected| setup.shows_cookie(&trailer, expected));
    let registered = if !allowed {
        Err("Authorization required: show the MIT-MAGIC-COOKIE-1")
    } else if setup.major == PROTOCOL_MAJOR {
        state
            .borrow_mut()
            .add_client(
                setup.endian,
                tx,
                Rc::clone(&pending),
                Rc::clone(&overflow),
                Rc::clone(&big_requests),
            )
            .ok_or("too many clients")
    } else {
        Err("protocol version not supported")
    };
    let (client, resource_base) = match registered {
        Ok(registered) => {
            state.borrow_mut().set_client_uid(registered.0, peer_uid);
            reader.allow_fds(MAX_QUEUED_FDS);
            registered
        }
        Err(reason) => {
            writer
                .write_all(&failure_reply(setup.endian, reason))
                .await?;
            return Ok(());
        }
    };

    tokio::task::spawn_local(write_loop(writer, rx, pending));
    {
        let state = state.borrow();
        let reply = success_reply(setup.endian, resource_base, state.screen_size());
        state.send_raw(client, reply);
    }
    let result = request_loop(
        &mut reader,
        setup.endian,
        client,
        &state,
        (&overflow, &big_requests),
    )
    .await;
    state.borrow_mut().remove_client(client);
    result
}

/// The setup request and its trailer (the authorization), which must arrive within `limit`.
async fn read_setup(
    reader: &mut ClientReader,
    limit: Duration,
) -> io::Result<(SetupRequest, Vec<u8>)> {
    let read = async {
        let mut header = [0u8; SETUP_HEADER_LEN];
        reader.read_exact(&mut header).await?;
        let setup = SetupRequest::parse_header(&header)
            .map_err(|_| invalid("bad connection setup request"))?;
        let mut trailer = vec![0u8; setup.trailer_len()];
        reader.read_exact(&mut trailer).await?;
        Ok((setup, trailer))
    };
    tokio::time::timeout(limit, read)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no connection setup in time"))?
}

async fn write_loop(
    mut writer: OwnedWriteHalf,
    mut rx: UnboundedReceiver<Vec<u8>>,
    pending: PendingBytes,
) {
    while let Some(bytes) = rx.recv().await {
        if writer.write_all(&bytes).await.is_err() {
            return;
        }
        pending.set(pending.get().saturating_sub(bytes.len()));
    }
}

async fn request_loop(
    reader: &mut ClientReader,
    endian: Endian,
    client: ClientId,
    state: &SharedState,
    (overflow, big_requests): (&Notify, &Cell<bool>),
) -> io::Result<()> {
    let mut handled: u32 = 0;
    loop {
        // Dropping a half-read request is fine here: the connection ends in that case.
        let request = tokio::select! {
            () = overflow.notified() => return Err(invalid("client does not read its output")),
            request = read_request(reader, endian, big_requests.get()) => request?,
        };
        let Some((head, body_len)) = request else {
            return Ok(());
        };
        // The descriptors that came with this request wait in the client's queue for it.
        let fds = reader.take_fds();
        if !fds.is_empty() && !state.borrow_mut().push_client_fds(client, fds) {
            return Err(invalid("too many file descriptors"));
        }
        let opcode = head[0];
        let body = reader.consume(body_len);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            state.borrow_mut().process(client, opcode, head[1], body);
        }));
        if outcome.is_err() {
            eprintln!(
                "xsrv-web: internal error while handling request {opcode}; closing the client"
            );
            return Err(invalid("internal error"));
        }
        // One thread serves everybody, so a client that sends a lot lets the others go first.
        handled = handled.wrapping_add(1);
        if handled.is_multiple_of(YIELD_EVERY) {
            tokio::task::yield_now().await;
        }
    }
}

/// Returns `None` when the client closed the connection between two requests.
/// After BIG-REQUESTS a length field of 0 is followed by a 32-bit length. Returns the header and
/// the length of the body, without the header or the extra length. The body then waits in the
/// reader (`ClientReader::consume`): a copy of each body cost one allocation for each request.
async fn read_request(
    reader: &mut ClientReader,
    endian: Endian,
    big_requests: bool,
) -> io::Result<Option<([u8; REQUEST_HEADER_LEN], usize)>> {
    let mut head = [0u8; REQUEST_HEADER_LEN];
    match reader.read_exact(&mut head).await {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    };
    let words = endian.read_u16([head[2], head[3]]) as usize;
    let body_len = if words != 0 {
        words * 4 - REQUEST_HEADER_LEN
    } else if big_requests {
        let mut extra = [0u8; 4];
        reader.read_exact(&mut extra).await?;
        let total = endian.read_u32(extra) as usize;
        let bytes = total
            .checked_mul(4)
            .filter(|bytes| (2 * REQUEST_HEADER_LEN..=MAX_BIG_REQUEST_BYTES).contains(bytes))
            .ok_or_else(|| invalid("bad big request length"))?;
        bytes - 2 * REQUEST_HEADER_LEN
    } else {
        return Err(invalid("zero request length"));
    };
    reader.buffer(body_len).await?;
    Ok(Some((head, body_len)))
}

/// Reads the bytes of a client, and the file descriptors that come with them (MIT-SHM
/// ShmAttachFd). One read can hold more than one request, so the bytes are buffered: the
/// unread bytes are `buffer[start..end]`.
struct ClientReader {
    half: OwnedReadHalf,
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    fds: Vec<OwnedFd>,
    /// The most descriptors that may wait here. 0 until the connection setup is accepted: a
    /// client could otherwise make the server hold descriptors without a limit.
    max_fds: usize,
}

impl ClientReader {
    fn new(half: OwnedReadHalf) -> Self {
        Self {
            half,
            buffer: Vec::new(),
            start: 0,
            end: 0,
            fds: Vec::new(),
            max_fds: 0,
        }
    }

    fn allow_fds(&mut self, max: usize) {
        self.max_fds = max;
    }

    /// Fills `out`, or fails with UnexpectedEof when the client closes first.
    async fn read_exact(&mut self, out: &mut [u8]) -> io::Result<()> {
        self.buffer(out.len()).await?;
        out.copy_from_slice(self.consume(out.len()));
        Ok(())
    }

    /// Waits until `len` unread bytes are in the buffer, or fails with UnexpectedEof.
    async fn buffer(&mut self, len: usize) -> io::Result<()> {
        while self.end - self.start < len {
            self.fill(len).await?;
        }
        Ok(())
    }

    /// The next `len` unread bytes. `buffer` must have made them ready.
    fn consume(&mut self, len: usize) -> &[u8] {
        let bytes = &self.buffer[self.start..self.start + len];
        self.start += len;
        bytes
    }

    /// The descriptors received so far.
    fn take_fds(&mut self) -> Vec<OwnedFd> {
        std::mem::take(&mut self.fds)
    }

    /// Reads once from the socket. `wanted` is how many unread bytes the caller waits for.
    async fn fill(&mut self, wanted: usize) -> io::Result<()> {
        self.make_room(wanted);
        let end = self.end;
        let stream = self.half.as_ref();
        let (read, fds) = loop {
            stream.readable().await?;
            let read = stream.try_io(Interest::READABLE, || {
                recv_with_fds(stream.as_raw_fd(), &mut self.buffer[end..])
            });
            match read {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                other => break other?,
            }
        };
        self.end += read;
        if self.fds.len() + fds.len() > self.max_fds {
            return Err(invalid("too many file descriptors"));
        }
        self.fds.extend(fds);
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }

    /// Moves the unread bytes to the front, and makes room for one more read: for all of the
    /// `wanted` bytes when a large request waits, so that it arrives in few reads. The buffer
    /// only grows while a request needs it; once it is all read, it goes back to one read's size.
    fn make_room(&mut self, wanted: usize) {
        if self.start == self.end {
            self.buffer.truncate(READ_CHUNK);
            self.buffer.shrink_to(READ_CHUNK);
        } else if self.start > 0 {
            self.buffer.copy_within(self.start..self.end, 0);
        }
        self.end -= self.start;
        self.start = 0;
        let room = (self.end + READ_CHUNK).max(wanted);
        if self.buffer.len() < room {
            self.buffer.resize(room, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::shm::send_with_fds;
    use super::*;
    use std::os::fd::AsFd;

    /// The server's reader, its write half (kept open), and the client's end.
    fn pair() -> (ClientReader, OwnedWriteHalf, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let (read, write) = UnixStream::from_std(ours).unwrap().into_split();
        (ClientReader::new(read), write, theirs)
    }

    fn file() -> OwnedFd {
        OwnedFd::from(std::fs::File::open("/dev/null").unwrap())
    }

    #[tokio::test]
    async fn a_client_that_sends_no_setup_is_closed_after_the_time_limit() {
        let (mut reader, _write, _client) = pair();
        let error = read_setup(&mut reader, Duration::from_millis(20))
            .await
            .err()
            .expect("no setup was sent");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn setup_slots_are_limited_and_given_back() {
        let pending = Rc::new(Cell::new(0));
        let slots: Vec<_> = (0..MAX_PENDING_SETUPS)
            .map(|_| SetupSlot::take(&pending).unwrap())
            .collect();
        assert!(SetupSlot::take(&pending).is_none());
        drop(slots);
        assert_eq!(pending.get(), 0);
        assert!(SetupSlot::take(&pending).is_some());
    }

    #[tokio::test]
    async fn a_descriptor_before_the_setup_ends_the_connection() {
        let (mut reader, _write, client) = pair();
        send_with_fds(&client, &[1, 2, 3, 4], &[file().as_fd()]);
        let error = reader.read_exact(&mut [0; 4]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn descriptors_over_the_limit_end_the_connection_inside_one_request() {
        let (mut reader, _write, client) = pair();
        reader.allow_fds(2);
        // One byte with each descriptor, as a client that wants the server to hold many would do.
        let (a, b, c) = (file(), file(), file());
        for fd in [&a, &b, &c] {
            send_with_fds(&client, &[0], &[fd.as_fd()]);
        }
        let error = reader.read_exact(&mut [0; 8]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn bytes_and_descriptors_within_the_limit_arrive() {
        let (mut reader, _write, client) = pair();
        reader.allow_fds(2);
        send_with_fds(&client, &[1, 2], &[file().as_fd()]);
        send_with_fds(&client, &[3, 4, 5], &[]);
        let mut out = [0; 4];
        reader.read_exact(&mut out).await.unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(reader.take_fds().len(), 1);
        let mut rest = [0; 1];
        reader.read_exact(&mut rest).await.unwrap();
        assert_eq!(rest, [5]);
    }
}
