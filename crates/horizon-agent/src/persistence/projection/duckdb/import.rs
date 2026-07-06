use anyhow::Result;

use crate::persistence::event_log::Record;

use super::{schema::CLEAR_ALL_AGENT_STATE_SQL, Store};

impl Store {
    pub fn replace_from_event_log_records(
        &self,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<()> {
        self.clear_all_agent_state()?;
        for record in records {
            self.append_record(&record)?;
        }
        Ok(())
    }

    fn clear_all_agent_state(&self) -> Result<()> {
        self.conn.execute_batch(CLEAR_ALL_AGENT_STATE_SQL)?;
        Ok(())
    }
}
