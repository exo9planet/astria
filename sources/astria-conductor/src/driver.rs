//! The driver is the top-level coordinator that runs and manages all the components
//! necessary for this reader/validator.

use std::time::Duration;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::{task, time};

use crate::executor::ExecutorCommand;
use crate::reader::ReaderCommand;
use crate::{
    alert::{AlertReceiver, AlertSender},
    conf::Conf,
    error::*,
    executor, reader,
};

pub async fn spawn(conf: Conf) -> Result<(DriverHandle, AlertReceiver)> {
    let (alert_tx, alert_rx) = mpsc::unbounded_channel();
    let (mut driver, tx) = Driver::new(conf, alert_tx).await?;

    let join_handle = task::spawn(async move { driver.run().await });

    Ok((
        DriverHandle {
            tx,
            join_handle: Some(join_handle),
        },
        alert_rx,
    ))
}

type JoinHandle = task::JoinHandle<Result<()>>;

pub struct DriverHandle {
    tx: Sender,
    join_handle: Option<JoinHandle>,
}

impl DriverHandle {
    /// Gracefully shuts down the driver and its components.
    /// Panics if the driver has already been shutdown.
    pub async fn shutdown(mut self) -> Result<()> {
        self.tx.send(DriverCommand::Shutdown)?;
        if let Err(e) = self
            .join_handle
            .take()
            .expect("Driver already shut down.")
            .await
            .expect("Task error.")
        {
            log::error!("Driver error: {}", e);
        }
        Ok(())
    }
}

/// The channel through which the user can send commands to the driver.
pub(crate) type Sender = UnboundedSender<DriverCommand>;
/// The channel on which the driver listens for commands from the user.
pub(crate) type Receiver = UnboundedReceiver<DriverCommand>;

/// The type of commands that the driver can receive.
#[allow(dead_code)] // TODO - remove after developing
#[derive(Debug)]
pub(crate) enum DriverCommand {
    /// Gracefully shuts down the driver and its components.
    Shutdown,
}

#[allow(dead_code)] // TODO - remove after developing
pub(crate) struct Driver {
    /// The channel on which other components in the driver sends the driver messages.
    cmd_rx: Receiver,

    /// The channel used to send messages to the reader task.
    reader_tx: reader::Sender,
    reader_join_handle: Option<reader::JoinHandle>,

    /// The channel used to send messages to the executor task.
    executor_tx: executor::Sender,
    executor_join_handle: Option<executor::JoinHandle>,

    /// The channel on which the driver and tasks in the driver can post alerts
    /// to the consumer of the driver.
    alert_tx: AlertSender,

    /// The global configuration
    conf: Conf,
}

impl Driver {
    async fn new(conf: Conf, alert_tx: AlertSender) -> Result<(Self, Sender)> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (reader_join_handle, reader_tx) = reader::spawn(&conf, cmd_tx.clone())?;
        let (executor_join_handle, executor_tx) = executor::spawn(&conf, cmd_tx.clone())?;

        // This task sends ReaderCommand::GetNewBlocks to reader_tx every 15 seconds.
        // 15 seconds was chosen because it is the Celestia block timing.
        let reader_tx_clone = reader_tx.clone();
        let forever_handle = task::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(15));
            loop {
                interval.tick().await;
                reader_tx_clone.send(ReaderCommand::GetNewBlocks).unwrap();
            }
        });
        forever_handle.await?;

        Ok((
            Self {
                cmd_rx,
                reader_tx,
                reader_join_handle: Some(reader_join_handle),
                executor_tx,
                executor_join_handle: Some(executor_join_handle),
                alert_tx,
                conf,
            },
            cmd_tx,
        ))
    }

    pub async fn run(&mut self) -> Result<()> {
        log::info!("Starting driver event loop.");
        while let Some(cmd) = self.cmd_rx.recv().await {
            match cmd {
                DriverCommand::Shutdown => {
                    self.shutdown().await?;
                    break;
                }
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        log::info!("Shutting down driver.");

        self.reader_tx.send(ReaderCommand::Shutdown)?;
        self.executor_tx.send(ExecutorCommand::Shutdown)?;

        Ok(())
    }
}