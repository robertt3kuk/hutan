//! Calls that wait for the desktop under test: `ui.run` is answered by the window, which only
//! runs while the test pumps GPUI, so the caller must not block the test thread.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::mpsc;

use hutan_client::{Client, ClientError};
use serde::de::DeserializeOwned;

use super::TestDaemon;
use hutan_protocol::messages::Request;

/// A request in flight on a connection of its own.
pub struct Pending<T> {
    rx: mpsc::Receiver<Result<T, ClientError>>,
}

impl<T> Pending<T> {
    /// The answer, once there is one.
    pub fn try_take(&self) -> Option<Result<T, ClientError>> {
        self.rx.try_recv().ok()
    }
}

impl TestDaemon {
    /// Sends `request` from a fresh connection without waiting for the answer.
    pub fn call_in_background<T: DeserializeOwned + Send + 'static>(
        &self,
        request: Request,
    ) -> Pending<T> {
        let options = self.options();
        let (tx, rx) = mpsc::channel();
        self.rt().spawn(async move {
            let client = Client::connect(options).await.unwrap();
            let _ = tx.send(client.call(request).await);
        });
        Pending { rx }
    }

    /// Sends `request` from a connection that says it runs inside `origin` (an agent's session,
    /// `HUTAN_SESSION_ID`), without waiting for the answer.
    pub fn call_from_in_background<T: DeserializeOwned + Send + 'static>(
        &self,
        origin: hutan_core::SessionId,
        request: Request,
    ) -> Pending<T> {
        let mut options = self.options();
        options.client.origin = Some(hutan_protocol::messages::Origin { session_id: origin });
        let (tx, rx) = mpsc::channel();
        self.rt().spawn(async move {
            let client = Client::connect(options).await.unwrap();
            let _ = tx.send(client.call(request).await);
        });
        Pending { rx }
    }

    /// Like [`TestDaemon::call`], keeping the error.
    pub fn try_call<T: DeserializeOwned + Send + 'static>(
        &self,
        request: Request,
    ) -> Result<T, ClientError> {
        let options = self.options();
        self.rt().block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await
        })
    }
}
