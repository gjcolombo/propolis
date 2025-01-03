// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Routines that manage connections to a VM's serial consoles.

use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use dropshot::WebsocketConnectionRaw;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use propolis::chardev::console::{
    ConsoleBackend, ReadOnlyClientHandle, ReadWriteClientHandle,
};
use propolis_api_types::InstanceSerialConsoleControlMessage;
use slog::{info, warn};
use tokio::{
    io::AsyncWriteExt,
    select,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::{
    tungstenite::{
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
    },
    WebSocketStream,
};

#[usdt::provider(provider = "propolis")]
mod probes {
    fn serial_event_done() {}
    fn serial_event_read(b: u8) {}
    fn serial_event_console_disconnect() {}
    fn serial_event_ws_recv(len: usize) {}
    fn serial_event_ws_error() {}
    fn serial_event_ws_disconnect() {}
    fn serial_event_wrote_byte(b: u8) {}
}

type ClientId = u64;

/// Indicates whether a serial console connection should be read-only. Read-only
/// connections are evicted if they fail to keep up with output bytes from the
/// console backend. Read-write connections will stall the console if they fall
/// behind while processing console output.
pub(crate) enum ReadOnly {
    ReadWrite,
    ReadOnly,
}

impl ReadOnly {
    fn is_readonly(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

enum ConsoleClient {
    ReadWrite(ReadWriteClientHandle),
    ReadOnly(#[allow(dead_code)] ReadOnlyClientHandle),
}

struct ClientTask {
    hdl: JoinHandle<()>,
    control_tx: mpsc::Sender<InstanceSerialConsoleControlMessage>,
    readonly: bool,
}

#[derive(Default)]
struct ClientTasks {
    tasks: BTreeMap<ClientId, ClientTask>,
    next_id: ClientId,
}

pub(crate) struct SerialConsoleManager {
    log: slog::Logger,

    /// The backend to which this manager's tasks connect.
    backend: Arc<ConsoleBackend>,

    /// A handle to the supervisor task for this manager. This task cleans up
    /// client connection tasks as they complete.
    supervisor_task: JoinHandle<()>,

    /// The set of client tasks this manager knows about. This is shared with
    /// the supervisor task, which reaps tasks as they exit.
    client_tasks: Arc<Mutex<ClientTasks>>,

    /// Exiting clients send their IDs to this channel to tell the supervisor to
    /// clean up their handles.
    client_done_tx: mpsc::Sender<ClientId>,

    /// Setting this to `true` signals to all tasks that they should terminate.
    done_tx: watch::Sender<bool>,

    /// Each new client task clones this receiver so it can learn when the
    /// manager has shut down.
    done_rx: watch::Receiver<bool>,
}

impl SerialConsoleManager {
    pub(crate) fn new(log: slog::Logger, backend: Arc<ConsoleBackend>) -> Self {
        let client_tasks = Arc::new(Mutex::new(ClientTasks::default()));
        let (client_done_tx, client_done_rx) = mpsc::channel(1);
        let (done_tx, done_rx) = watch::channel(false);

        let log_for_supervisor = log.clone();
        let client_for_supervisor = client_tasks.clone();
        let done_for_supervisor = done_rx.clone();
        let supervisor_task = tokio::spawn(async move {
            supervisor_task(
                log_for_supervisor,
                done_for_supervisor,
                client_for_supervisor,
                client_done_rx,
            )
            .await;
        });

        Self {
            log,
            backend,
            supervisor_task,
            client_tasks,
            client_done_tx,
            done_tx,
            done_rx,
        }
    }

    pub(crate) async fn finish(self) {
        self.done_tx.send(true).expect("manager owns a copy of done_rx");
        let _ = self.supervisor_task.await;
    }

    pub(crate) fn connect(
        &self,
        ws: WebSocketStream<WebsocketConnectionRaw>,
        readonly: ReadOnly,
    ) {
        // Read-only clients disconnect if they aren't able to keep up with
        // incoming bytes from the guest. Create a slightly larger channel for
        // them to allow some buffering of incoming guest bytes.
        let ch_size = match readonly {
            ReadOnly::ReadWrite => 1,
            ReadOnly::ReadOnly => 256,
        };

        let (console_tx, console_rx) = mpsc::channel(ch_size);
        let console_client = match readonly {
            ReadOnly::ReadWrite => ConsoleClient::ReadWrite(
                self.backend.attach_rw_client(console_tx),
            ),
            ReadOnly::ReadOnly => ConsoleClient::ReadOnly(
                self.backend.attach_ro_client(console_tx),
            ),
        };

        let mut client_tasks = self.client_tasks.lock().unwrap();
        let client_id = client_tasks.next_id;
        client_tasks.next_id += 1;
        let (control_tx, control_rx) = mpsc::channel(1);

        let ctx = SerialTaskContext {
            log: self.log.clone(),
            ws,
            console_client,
            console_rx,
            control_rx,
            done_rx: self.done_rx.clone(),
            client_done_tx: self.client_done_tx.clone(),
            client_id,
        };

        let task = ClientTask {
            hdl: tokio::spawn(async move { serial_task(ctx).await }),
            control_tx,
            readonly: readonly.is_readonly(),
        };

        client_tasks.tasks.insert(client_id, task);
    }

    pub(crate) async fn notify_migration(&self, destination: SocketAddr) {
        let from_start = self.backend.bytes_since_start() as u64;
        let entries: Vec<_> = {
            let clients = self.client_tasks.lock().unwrap();
            clients
                .tasks
                .values()
                .map(|client| (client.control_tx.clone(), client.readonly))
                .collect()
        };

        for entry in entries {
            let _ = entry
                .0
                .send(InstanceSerialConsoleControlMessage::Migrating {
                    destination,
                    from_start,
                    readonly: entry.1,
                })
                .await;
        }
    }
}

async fn supervisor_task(
    log: slog::Logger,
    mut done_rx: watch::Receiver<bool>,
    client_tasks: Arc<Mutex<ClientTasks>>,
    mut client_done_rx: mpsc::Receiver<ClientId>,
) {
    enum Event {
        Done,
        ClientDone(ClientId),
    }
    loop {
        let event = select! {
            biased;

            _ = done_rx.changed() => {
                Event::Done
            }

            client_done = client_done_rx.recv() => {
                Event::ClientDone(
                    client_done.expect("child_done_tx owned by manager")
                )
            }
        };

        match event {
            Event::Done => {
                info!(log, "serial console supervisor task shutting down");
                let tasks = {
                    let mut guard = client_tasks.lock().unwrap();
                    std::mem::take(&mut guard.tasks)
                };

                let tasks: Vec<_> = tasks.into_values().collect();
                futures::future::join_all(
                    tasks.into_iter().map(|task| task.hdl),
                )
                .await;
                break;
            }

            Event::ClientDone(id) => {
                let task =
                    client_tasks.lock().unwrap().tasks.remove(&id).expect(
                        "clients must be registered when they complete",
                    );

                let _ = task.hdl.await;
            }
        }
    }
}

struct SerialTaskContext {
    log: slog::Logger,
    ws: WebSocketStream<WebsocketConnectionRaw>,
    console_client: ConsoleClient,
    console_rx: mpsc::Receiver<u8>,
    control_rx: mpsc::Receiver<InstanceSerialConsoleControlMessage>,
    done_rx: watch::Receiver<bool>,
    client_done_tx: mpsc::Sender<ClientId>,
    client_id: ClientId,
}

async fn serial_task(
    SerialTaskContext {
        log,
        ws,
        mut console_client,
        mut console_rx,
        mut control_rx,
        mut done_rx,
        client_done_tx,
        client_id,
    }: SerialTaskContext,
) {
    enum Event {
        Done,
        ConsoleRead(u8),
        ConsoleDisconnected,
        WroteToBackend(Result<usize, (std::io::Error, &'static str)>),
        ControlMessage(InstanceSerialConsoleControlMessage),
        WebsocketMessage(Message),
        WebsocketError(tokio_tungstenite::tungstenite::Error),
        WebsocketDisconnected,
    }

    async fn close(
        log: &slog::Logger,
        client_id: ClientId,
        sink: SplitSink<WebSocketStream<WebsocketConnectionRaw>, Message>,
        stream: SplitStream<WebSocketStream<WebsocketConnectionRaw>>,
        reason: &str,
    ) {
        let mut ws =
            sink.reunite(stream).expect("sink and stream should match");
        if let Err(e) = ws
            .close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: reason.into(),
            }))
            .await
        {
            warn!(
                log, "error sending close frame to client";
                "client_id" => client_id,
                "error" => ?e,
            );
        }
    }

    let readonly = match console_client {
        ConsoleClient::ReadWrite(_) => false,
        ConsoleClient::ReadOnly(_) => true,
    };

    info!(
        log,
        "serial console task started";
        "client_id" => client_id,
        "readonly" => readonly
    );

    let mut remaining_to_send: VecDeque<u8> = VecDeque::new();
    let (mut sink, mut stream) = ws.split();
    let mut close_reason: Option<&'static str> = None;
    loop {
        use futures::future::Either;

        // If the client is a read-write client and there are bytes available to
        // send to the guest, construct a future that will actually send them.
        let (will_send, send_fut) =
            if let (ConsoleClient::ReadWrite(hdl), false) =
                (&mut console_client, remaining_to_send.is_empty())
            {
                // Ensure that all available bytes can be offered in a single
                // slice. This should generally not be very expensive because
                // when the deque has contents, those contents will be
                // completely drained before any new bytes can be read from the
                // websocket.
                remaining_to_send.make_contiguous();
                (
                    true,
                    Either::Left(write_to_backend(
                        hdl,
                        remaining_to_send.as_slices().0,
                    )),
                )
            } else {
                (false, Either::Right(futures::future::pending()))
            };

        // If there are no bytes to be sent to the guest, accept another message
        // from the websocket.
        let ws_fut = if !will_send {
            Either::Left(stream.next())
        } else {
            Either::Right(futures::future::pending())
        };

        let event = select! {
            // The priority of these branches is important:
            //
            // 1. Requests to stop the client take precedence over everything
            //    else.
            // 2. New bytes written by the guest need to be processed before any
            //    other requests: if a guest outputs a byte while a read-write
            //    client is attached, the relevant vCPU will be blocked until
            //    the client processes the byte.
            biased;

            _ = done_rx.changed() => {
                Event::Done
            }

            res = console_rx.recv() => {
                match res {
                    Some(b) => Event::ConsoleRead(b),
                    None => Event::ConsoleDisconnected,
                }
            }

            control = control_rx.recv() => {
                Event::ControlMessage(control.expect(
                    "serial control channel should outlive its task"
                ))
            }

            res = send_fut => {
                Event::WroteToBackend(res)
            }

            ws = ws_fut => {
                match ws {
                    None => Event::WebsocketDisconnected,
                    Some(Ok(msg)) => Event::WebsocketMessage(msg),
                    Some(Err(err)) => Event::WebsocketError(err),
                }
            }
        };

        match event {
            Event::Done => {
                probes::serial_event_done!(|| ());
                close_reason = Some("VM stopped");
                break;
            }
            Event::ConsoleRead(b) => {
                probes::serial_event_read!(|| (b));

                // Waiting outside the `select!` is OK here:
                //
                // - If the client is a read-write client, it is allowed to
                //   block the guest to ensure that every byte of guest output
                //   is transmitted to the client.
                // - If the client is a read-only client, and it is slow to
                //   acknowledge this message, its channel to the backend will
                //   eventually fill up. If this happens and the backend thus
                //   becomes unable to send new bytes, it will drop the channel
                //   to allow the guest to make progress.
                let _ = sink.send(Message::binary(vec![b])).await;
            }
            Event::ConsoleDisconnected => {
                probes::serial_event_console_disconnect!(|| ());
                info!(
                    log, "console backend dropped its client channel";
                    "client_id" => client_id
                );
                break;
            }
            Event::ControlMessage(control) => {
                let _ = sink
                    .send(Message::Text(
                        serde_json::to_string(&control).expect(
                            "control messages can always serialize into JSON",
                        ),
                    ))
                    .await;
            }
            Event::WroteToBackend(result) => {
                let written = match result {
                    Ok(n) => n,
                    Err((e, reason)) => {
                        warn!(
                            log,
                            "dropping read-write console client";
                            "client_id" => client_id,
                            "error" => ?e,
                            "reason" => reason
                        );

                        close_reason = Some(reason);
                        break;
                    }
                };

                drop(remaining_to_send.drain(..written));
            }
            Event::WebsocketMessage(msg) => match (&mut console_client, msg) {
                (ConsoleClient::ReadWrite(_), Message::Binary(bytes)) => {
                    probes::serial_event_ws_recv!(|| (bytes.len()));
                    remaining_to_send.extend(bytes.as_slice());
                }
                (ConsoleClient::ReadOnly(_), Message::Binary(_)) => {
                    continue;
                }
                (_, _) => continue,
            },
            Event::WebsocketError(e) => {
                probes::serial_event_ws_error!(|| ());
                warn!(
                    log, "serial console websocket error";
                    "client_id" => client_id,
                    "error" => ?e
                );
                break;
            }
            Event::WebsocketDisconnected => {
                probes::serial_event_ws_disconnect!(|| ());
                info!(
                    log, "serial console client disconnected";
                    "client_id" => client_id
                );
                break;
            }
        }
    }

    info!(log, "serial console task exiting"; "client_id" => client_id);
    if let Some(close_reason) = close_reason {
        close(&log, client_id, sink, stream, close_reason).await;
    }

    let _ = client_done_tx.send(client_id).await;
}

/// Attempts to write `bytes` to the console backend via the supplied read-write
/// client handle. Failure to write bytes is presumed to give the caller cause
/// to disconnect the client.
///
/// # Return value
///
/// Returns the number of bytes written on success. On failure, returns the I/O
/// error produced by the handle (for logging purposes) and a friendly
/// disconnection reason string for the caller to pass back to the websocket
/// client.
///
/// # Cancel safety
///
/// The future produced by this function call is cancel-safe: if it is dropped,
/// it is guaranteed that no bytes were written to the console. See
/// [`ReadWriteClientHandle`]'s documentation for more details.
async fn write_to_backend(
    hdl: &mut ReadWriteClientHandle,
    bytes: &[u8],
) -> Result<usize, (std::io::Error, &'static str)> {
    let written = hdl.write(bytes).await.map_err(|e| {
        let reason = if e.kind() == std::io::ErrorKind::ConnectionAborted {
            "read-write console connection overtaken"
        } else {
            "error writing to console backend"
        };

        (e, reason)
    })?;

    for byte in bytes.iter().take(written) {
        probes::serial_event_wrote_byte!(|| (byte));
    }

    Ok(written)
}
