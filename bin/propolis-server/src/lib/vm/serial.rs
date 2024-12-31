// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Routines that manage connections to a VM's serial consoles.
//!
//! Incoming calls to the `/instance/serial` endpoint produce a websocket stream
//! that gets handed off to a connection task. This task is responsible for
//! reading to and writing from the socket and for interfacing with the Propolis
//! console backend.

use std::{
    collections::BTreeMap,
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
    fn serial_event_ws_recv() {}
    fn serial_event_ws_error() {}
    fn serial_event_ws_disconnect() {}
    fn serial_event_wrote_byte(b: u8) {}
type ClientId = u64;

/// Indicates whether a serial console connection should be read-only. Read-only
/// connections are evicted if they fail to keep up with output bytes from the
/// console backend. Read-write connections will stall the console if they fall
/// behind while processing console output.
pub(crate) enum ReadOnly {
    ReadWrite,
    ReadOnly,
}

enum ConsoleClient {
    ReadWrite(ReadWriteClientHandle),
    ReadOnly(ReadOnlyClientHandle),
}

#[derive(Default)]
struct ClientTasks {
    tasks: BTreeMap<ClientId, JoinHandle<()>>,
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
        // Read-only clients disconnect as soon as they
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
        let ctx = SerialTaskContext {
            log: self.log.clone(),
            ws,
            console_client,
            console_rx,
            done_rx: self.done_rx.clone(),
            client_done_tx: self.client_done_tx.clone(),
            client_id,
        };

        let task = tokio::spawn(async move { serial_task(ctx).await });
        client_tasks.tasks.insert(client_id, task);
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
                futures::future::join_all(tasks.into_iter()).await;
                break;
            }

            Event::ClientDone(id) => {
                let task =
                    client_tasks.lock().unwrap().tasks.remove(&id).expect(
                        "clients must be registered when they complete",
                    );

                let _ = task.await;
            }
        }
    }
}

struct SerialTaskContext {
    log: slog::Logger,
    ws: WebSocketStream<WebsocketConnectionRaw>,
    console_client: ConsoleClient,
    console_rx: mpsc::Receiver<u8>,
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
        mut done_rx,
        client_done_tx,
        client_id,
    }: SerialTaskContext,
) {
    enum Event {
        Done,
        ConsoleRead(u8),
        ConsoleDisconnected,
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

    info!(log, "serial console task started"; "client_id" => client_id);
    let (mut sink, mut stream) = ws.split();
    let mut close_reason: Option<&'static str> = None;
    loop {
        let event = select! {
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

            ws = stream.next() => {
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
            Event::WebsocketMessage(msg) => match (&mut console_client, msg) {
                (ConsoleClient::ReadWrite(hdl), Message::Binary(bytes)) => {
                    probes::serial_event_ws_recv!(|| ());
                    let mut bytes = bytes.as_slice();
                    while !bytes.is_empty() {
                        use std::io::ErrorKind;
                        let written = match hdl.write(bytes).await {
                            Ok(n) => n,
                            Err(e)
                                if e.kind() == ErrorKind::ConnectionAborted =>
                            {
                                info!(
                                    log,
                                    "read-write console connection overtaken";
                                    "client_id" => client_id,
                                );

                                close_reason = Some(
                                    "connection taken over by another client",
                                );
                                break;
                            }
                            Err(e) => {
                                warn!(
                                    log,
                                    "error writing to console backend";
                                    "client_id" => client_id,
                                    "error" => ?e
                                );

                                close_reason = Some(
                                    "internal error writing to console backend",
                                );
                                break;
                            }
                        };

                        probes::serial_event_wrote_byte!(|| (bytes[0]));
                        bytes = &bytes[written..];
                    }
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
