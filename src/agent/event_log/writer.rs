use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use super::{read_agent_event_log, AgentEventLogRecord};

#[derive(Clone)]
pub struct AgentEventLogWriterHandle {
    tx: Sender<AgentEventLogWriterCommand>,
    next_sequence: Arc<Mutex<u64>>,
}

impl AgentEventLogWriterHandle {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("create agent event log directory {}", parent.display())
                })?;
            }
        }

        let next_sequence = read_agent_event_log(path)?
            .records
            .iter()
            .map(|record| record.sequence)
            .max()
            .map(|sequence| sequence + 1)
            .unwrap_or(0);
        let (tx, rx) = unbounded();
        let writer_path = path.to_path_buf();
        thread::spawn(move || run_writer(writer_path, rx));

        Ok(Self {
            tx,
            next_sequence: Arc::new(Mutex::new(next_sequence)),
        })
    }

    pub fn append(&self, mut record: AgentEventLogRecord) -> Result<()> {
        {
            let mut next_sequence = self
                .next_sequence
                .lock()
                .map_err(|_| anyhow::anyhow!("agent event log sequence lock poisoned"))?;
            record.sequence = *next_sequence;
            *next_sequence += 1;
        }
        self.tx
            .send(AgentEventLogWriterCommand::Append(record))
            .context("enqueue agent event log record")
    }

    #[cfg(test)]
    pub fn flush_for_tests(&self) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(AgentEventLogWriterCommand::Flush(tx))
            .context("enqueue agent event log flush")?;
        rx.recv().context("wait for agent event log flush")?
    }
}

enum AgentEventLogWriterCommand {
    Append(AgentEventLogRecord),
    #[cfg(test)]
    Flush(Sender<Result<()>>),
}

fn run_writer(path: PathBuf, rx: Receiver<AgentEventLogWriterCommand>) {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let mut writer = BufWriter::new(file);

    while let Ok(command) = rx.recv() {
        match command {
            AgentEventLogWriterCommand::Append(record) => {
                if serde_json::to_writer(&mut writer, &record).is_ok() {
                    let _ = writer.write_all(b"\n");
                }
            }
            #[cfg(test)]
            AgentEventLogWriterCommand::Flush(reply) => {
                let result = writer
                    .flush()
                    .with_context(|| format!("flush agent event log {}", path.display()));
                let _ = reply.send(result);
            }
        }
    }

    let _ = writer.flush();
}
