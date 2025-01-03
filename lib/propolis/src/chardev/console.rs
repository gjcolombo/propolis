// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Objects used to treat a character device (like a serial port) as an
//! interactive console.
//!
//! Console backends have the following semantics, described in more detail in
//! RFD 491:
//!
//! - The console maintains a history of the last N bytes that the device
//!   outputted to it.
//! - A console may have at most one "interactive" client who can write bytes
//!   into the device.
//!   - While an interactive client is connected, it limits the rate at which
//!     new bytes from the device can be buffered: every new byte must be
//!     written to the client (assuming the client stays alive) before it can
//!     be written to the history buffer.
//!   - If no interactive client is connected, bytes are written to the buffer
//!     as fast as they can be processed.
//!   - If a new interactive client connects to the console while one is already
//!     connected, the old client is evicted.
//! - A console may also have zero or more non-interactive read-only clients
//!   who receive bytes as the device issues them. If a client is not ready to
//!   receive a byte when the device issues it, the console disconnects the
//!   client immediately.

use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use tokio::{io::AsyncWrite, sync::mpsc};

use crate::{
    common::Lifecycle,
    migrate::{
        MigrateCtx, MigrateSingle, MigrateStateError, Migrator, PayloadOutput,
    },
};

use super::{
    history_buffer::{
        migrate::ConsoleHistoryBufferV1, HistoryBuffer, SerialHistoryOffset,
    },
    Sink, Source,
};

type ClientId = u64;

pub trait ConsoleDevice: Source + Sink {}

/// Represents a read-only client connection to the console.
struct ReadOnlyClient {
    /// A channel to which new bytes from the device should be sent.
    tx: mpsc::Sender<u8>,
}

/// A handle that identifies a specific read-only client connection to this
/// console. Dropping this handle disconnects the client.
pub struct ReadOnlyClientHandle {
    id: ClientId,
    backend: Arc<ConsoleBackend>,
}

impl Drop for ReadOnlyClientHandle {
    fn drop(&mut self) {
        let mut inner = self.backend.inner.lock().unwrap();
        inner.ro_clients.remove(&self.id);
    }
}

/// Represents a read-write client connection to the console.
struct ReadWriteClient {
    /// The handle value assigned to this client when it connected to this
    /// console.
    id: ClientId,

    /// A channel to which new bytes from the device should be sent.
    tx: mpsc::Sender<u8>,

    /// A waker to signal when this session is ready to accept a new writable
    /// byte.
    write_waker: Option<Waker>,
}

/// A handle that identifies a specific read-write client for a console device.
/// Dropping this handle disconnects the client.
///
/// Read-write clients implement [`tokio::io::AsyncWrite`]; clients write to
/// the console by attempting to write directly to the handle.
pub struct ReadWriteClientHandle {
    id: ClientId,
    backend: Arc<ConsoleBackend>,
}

impl Drop for ReadWriteClientHandle {
    fn drop(&mut self) {
        let mut inner = self.backend.inner.lock().unwrap();
        if let Some(client) = inner.rw_client.take() {
            if client.id != self.id {
                inner.rw_client = Some(client)
            }
        }
    }
}

/// Console backend data that must be accessed under a lock.
struct Inner {
    /// A buffer containing the most recent bytes written to the console.
    buffer: HistoryBuffer,

    /// The currently-connected read-write client, if there is one.
    rw_client: Option<ReadWriteClient>,

    /// The set of connected read-only clients, indexed by an integer identifier
    /// (to allow disconnection of one client without disrupting others).
    ro_clients: BTreeMap<ClientId, ReadOnlyClient>,

    /// The next read-only client identifier to try to assign.
    next_client_id: ClientId,
}

impl Inner {
    fn new(buffer_size: usize) -> Self {
        Self {
            buffer: HistoryBuffer::new(buffer_size),
            rw_client: None,
            ro_clients: BTreeMap::new(),
            next_client_id: 0,
        }
    }

    fn next_client_handle(&mut self) -> u64 {
        let hdl = self.next_client_id;
        self.next_client_id += 1;
        hdl
    }

    /// If the current read-write client's handle value is `id`, returns a
    /// mutable reference to that client, returning `None` otherwise.
    fn rw_client_mut(&mut self, id: ClientId) -> Option<&mut ReadWriteClient> {
        match self.rw_client.as_mut() {
            Some(client) if client.id == id => Some(client),
            _ => None,
        }
    }
}

/// A serial console backend.
pub struct ConsoleBackend {
    inner: Mutex<Inner>,
    dev: Arc<dyn ConsoleDevice>,
}

impl ConsoleBackend {
    pub fn new(buffer_size: usize, dev: &Arc<dyn ConsoleDevice>) -> Arc<Self> {
        let this = Arc::new(Self {
            inner: Mutex::new(Inner::new(buffer_size)),
            dev: dev.clone(),
        });

        let read_notifier = this.clone();
        dev.set_autodiscard(false);
        Source::set_notifier(
            dev.as_ref(),
            Some(Box::new(move |s| read_notifier.notify_read(s))),
        );

        let write_notifier = this.clone();
        Sink::set_notifier(
            dev.as_ref(),
            Some(Box::new(move |s| write_notifier.notify_write(s))),
        );

        this
    }

    /// Attaches a new read-write client to this console backend.
    ///
    /// The `tx` argument supplies a channel to which the backend should send
    /// bytes as they arrive from the backend's associated console device. If a
    /// byte arrives from the device and `tx` is full, device processing will
    /// block until the byte can be sent or the receiver half of the channel is
    /// dropped.
    ///
    /// If the backend already has a client, it is replaced with the new client,
    /// and the previous client and its transmission channel are dropped.
    pub fn attach_rw_client(
        self: &Arc<Self>,
        tx: mpsc::Sender<u8>,
    ) -> ReadWriteClientHandle {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_client_handle();
        inner.rw_client = Some(ReadWriteClient { id, tx, write_waker: None });

        ReadWriteClientHandle { id, backend: self.clone() }
    }

    /// Attaches a new read-only client to this console backend.
    ///
    /// The `tx` argument supplied a channel to which the backend should try to
    /// send bytes as they arrive from the backend's associated console device.
    /// If a byte arrives from the device and `tx` is full, this client is
    /// disconnected from the backend and its `tx` channel is dropped. Callers
    /// should size their transmission buffers to avoid this if necessary.
    pub fn attach_ro_client(
        self: &Arc<Self>,
        tx: mpsc::Sender<u8>,
    ) -> ReadOnlyClientHandle {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_client_handle();
        inner.ro_clients.insert(id, ReadOnlyClient { tx });
        ReadOnlyClientHandle { id, backend: self.clone() }
    }

    pub fn history_vec(
        &self,
        byte_offset: SerialHistoryOffset,
        max_bytes: Option<usize>,
    ) -> Result<(Vec<u8>, usize), super::history_buffer::Error> {
        let inner = self.inner.lock().unwrap();
        inner.buffer.contents_vec(byte_offset, max_bytes)
    }

    pub fn bytes_since_start(&self) -> usize {
        self.inner.lock().unwrap().buffer.bytes_from_start()
    }

    /// Invoked in response to a read-ready notification from the backend's
    /// associated device.
    fn notify_read(&self, source: &dyn Source) {
        struct RoClient {
            id: ClientId,
            tx: mpsc::Sender<u8>,
            dead: bool,
        }

        let Some(c) = source.read() else {
            return;
        };

        // Take the lock and capture all listeners for this byte, then drop the
        // lock before actually dispatching the byte to anyone.
        let (rw_tx, mut ro_clients) = {
            let inner = self.inner.lock().unwrap();
            let rw_tx = inner.rw_client.as_ref().map(|c| c.tx.clone());
            let ro_clients = inner
                .ro_clients
                .iter()
                .map(|(id, c)| RoClient {
                    id: *id,
                    tx: c.tx.clone(),
                    dead: false,
                })
                .collect::<Vec<_>>();
            (rw_tx, ro_clients)
        };

        // It's not safe to hold the lock while issuing a blocking send to the
        // read-write client, because that client might simultaneously be
        // issuing a write that needs to take the lock.
        let rw_dead = if let Some(rw_tx) = rw_tx {
            rw_tx.blocking_send(c).is_err()
        } else {
            false
        };

        for client in ro_clients.iter_mut() {
            if client.tx.try_send(c).is_err() {
                client.dead = true;
            }
        }

        let mut inner = self.inner.lock().unwrap();
        inner.buffer.consume(&[c]);
        if rw_dead {
            inner.rw_client = None;
        }

        for client in ro_clients.iter().filter(|client| client.dead) {
            inner.ro_clients.remove(&client.id);
        }
    }

    /// Invoked in response to a write-ready notification from the backend's
    /// associated device.
    fn notify_write(&self, _sink: &dyn Sink) {
        // Take the lock and see if there's a waker from a previous attempt to
        // write that went unfulfilled. If so, wake it so it can try to write
        // again. This write attempt should now succeed, since there can only be
        // one read-write client connected at a time.
        //
        // If another client replaced the read-write client that registered for
        // this notification, this wakeup is useless, since the old client will
        // no longer be able to write anything. This is OK, however, because the
        // sink remains ready and the new client will be able to write to it
        // immediately.
        let mut inner = self.inner.lock().unwrap();
        if let Some(rw_client) = inner.rw_client.as_mut() {
            if let Some(waker) = rw_client.write_waker.take() {
                waker.wake()
            }
        }
    }
}

impl AsyncWrite for ReadWriteClientHandle {
    /// Attempts to write bytes from `buf` into the console to which this client
    /// is connected. Returns an error if this client is no longer the active
    /// read-write client for this console.
    ///
    /// # Cancel safety
    ///
    /// This routine is cancel-safe: if it returns `Pending` and the
    /// corresponding future is dropped, it is guaranteed that no bytes of `buf`
    /// were ever written to the console device.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let mut inner = self.backend.inner.lock().unwrap();
        let Some(inner_client) = inner.rw_client_mut(self.id) else {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        };

        if self.backend.dev.write(buf[0]) {
            Poll::Ready(Ok(1))
        } else {
            inner_client.write_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Lifecycle for ConsoleBackend {
    fn type_name(&self) -> &'static str {
        "console"
    }

    fn migrate(&self) -> Migrator {
        Migrator::Single(self)
    }

    // TODO(gjc) think about how to handle pause...
}

impl MigrateSingle for ConsoleBackend {
    fn export(
        &self,
        _ctx: &MigrateCtx,
    ) -> Result<PayloadOutput, MigrateStateError> {
        Ok(self.inner.lock().unwrap().buffer.export().into())
    }

    fn import(
        &self,
        mut offer: crate::migrate::PayloadOffer,
        _ctx: &MigrateCtx,
    ) -> Result<(), MigrateStateError> {
        let data: ConsoleHistoryBufferV1 = offer.parse()?;
        self.inner.lock().unwrap().buffer.import(data);
        Ok(())
    }
}
