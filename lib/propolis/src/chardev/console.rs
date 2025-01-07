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
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use crate::{
    chardev::{
        history_buffer::{
            migrate::ConsoleHistoryBufferV1, HistoryBuffer, SerialHistoryOffset,
        },
        pollers::SinkBuffer,
        Sink, Source,
    },
    common::Lifecycle,
    migrate::{
        MigrateCtx, MigrateSingle, MigrateStateError, Migrator, PayloadOutput,
    },
};

type ClientId = u64;

/// A device acting as a console must be a character source and sink.
pub trait ConsoleDevice: Source + Sink {
    /// Upcasts the console device to a reference to a `Sink`.
    fn upcast_sink(&self) -> &dyn Sink;

    /// Upcasts an `Arc<ConsoleDevice>` to an `Arc<Sink>`.
    fn upcast_arc_sink(self: Arc<Self>) -> Arc<dyn Sink>;
}

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
    sink_buffer: Arc<SinkBuffer>,
}

impl ConsoleBackend {
    pub fn new(buffer_size: usize, dev: &Arc<dyn ConsoleDevice>) -> Arc<Self> {
        let sink = SinkBuffer::new(NonZeroUsize::new(64).unwrap());
        sink.attach(dev.clone().upcast_arc_sink().as_ref());

        let this = Arc::new(Self {
            inner: Mutex::new(Inner::new(buffer_size)),
            dev: dev.clone(),
            sink_buffer: sink,
        });

        let read_notifier = this.clone();
        dev.set_autodiscard(false);
        Source::set_notifier(
            dev.as_ref(),
            Some(Box::new(move |s| read_notifier.notify_read(s))),
        );
        this
    }

    /// Attaches a new read-write client to this console backend.
    ///
    /// Bytes arriving from the guest will be written to `tx`. If this channel
    /// is full, the backend will block until all bytes can be sent or the
    /// receiver half of the channel is dropped.
    ///
    /// If the backend already has a client, it is replaced with the new client,
    /// and the previous client and its transmission channel are dropped.
    pub fn attach_rw_client(
        self: &Arc<Self>,
        tx: mpsc::Sender<u8>,
    ) -> ReadWriteClientHandle {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_client_handle();
        inner.rw_client = Some(ReadWriteClient { id, tx });

        ReadWriteClientHandle { id, backend: self.clone() }
    }

    /// Attaches a new read-only client to this console backend.
    ///
    /// Bytes arriving from the guest will be written to `tx`. If this channel
    /// is full, this client is removed from the backend's client list and its
    /// channel is dropped, disconnecting the client.
    pub fn attach_ro_client(
        self: &Arc<Self>,
        tx: mpsc::Sender<u8>,
    ) -> ReadOnlyClientHandle {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_client_handle();
        inner.ro_clients.insert(id, ReadOnlyClient { tx });
        ReadOnlyClientHandle { id, backend: self.clone() }
    }

    /// Obtains a history of bytes sent to this console session, starting at
    /// `byte_offset` and returning up to `max_bytes` (if specified).
    ///
    /// # Return value
    ///
    /// A tuple whose first element is a vector of bytes and whose second
    /// element is the number of bytes that were recorded from instance start up
    /// to and including the last byte in the output vector.
    pub fn history_vec(
        &self,
        byte_offset: SerialHistoryOffset,
        max_bytes: Option<usize>,
    ) -> Result<(Vec<u8>, usize), super::history_buffer::Error> {
        let inner = self.inner.lock().unwrap();
        inner.buffer.contents_vec(byte_offset, max_bytes)
    }

    /// Yields the number of bytes recorded to this console since it was
    /// started.
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
        // lock before actually dispatching the byte to anyone. It's not safe to
        // hold the lock while sending blocking sends to the read-write client,
        // because it might simultaneously be issuing a write that needs to take
        // the lock.
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

        let rw_dead = rw_tx.map_or(false, |tx| tx.blocking_send(c).is_err());
        for client in ro_clients.iter_mut() {
            if client.tx.try_send(c).is_err() {
                client.dead = true;
            }
        }

        // Retake the lock and drop any clients that have closed their channels
        // or that weren't able to accept the new byte.
        let mut inner = self.inner.lock().unwrap();
        inner.buffer.consume(&[c]);
        if rw_dead {
            inner.rw_client = None;
        }

        for client in ro_clients.iter().filter(|client| client.dead) {
            inner.ro_clients.remove(&client.id);
        }
    }
}

impl ReadWriteClientHandle {
    /// Writes bytes from `buf` to the console.
    ///
    /// # Return value
    ///
    /// - On success, returns the number of bytes written.
    /// - On failure, returns [`std::io::ErrorKind::ConnectionAborted`] to
    ///   indicate that the caller is no longer the read-write client for this
    ///   console.
    ///
    /// # Cancel safety
    ///
    /// The future returned by this function is cancel-safe: if it is dropped
    /// before completion, it is guaranteed that no data was written to the
    /// console. See [`SinkBuffer::write`].
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Don't accept this write if this client is no longer the active R/W
        // client (it may not yet have noticed that it was overtaken).
        {
            let mut inner = self.backend.inner.lock().unwrap();
            if inner.rw_client_mut(self.id).is_none() {
                return Err(std::io::Error::from(
                    std::io::ErrorKind::ConnectionAborted,
                ));
            };
        }

        Ok(self
            .backend
            .sink_buffer
            .write(buf, self.backend.dev.upcast_sink())
            .await
            .unwrap())
    }
}

impl Lifecycle for ConsoleBackend {
    fn type_name(&self) -> &'static str {
        "console"
    }

    fn migrate(&self) -> Migrator {
        Migrator::Single(self)
    }
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
