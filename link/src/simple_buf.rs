use anyhow::Result;
use std::fmt;

pub struct SimpleBuf {
    data_vec: Vec<u8>,
    capacity: usize,
    vacant_idx: usize,
    occupied_idx: usize,
}

impl SimpleBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            data_vec: vec![0u8; capacity],
            capacity,
            vacant_idx: 0,
            occupied_idx: 0,
        }
    }
}

impl SimpleBuf {
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.vacant_idx - self.occupied_idx
    }

    pub fn max_len(&self) -> usize {
        self.capacity - (self.vacant_idx - self.occupied_idx)
    }

    pub fn push_slice(&mut self, b: &[u8]) -> Result<()> {
        let usable = self.vacant_slice().len();
        if usable < b.len() {
            anyhow::bail!(
                "simple buf vacant slice len {} < b len {} should not happen",
                usable,
                b.len()
            );
        }
        self.data_vec[self.vacant_idx..self.vacant_idx + b.len()].copy_from_slice(b);
        self.vacant_idx += b.len();
        Ok(())
    }

    pub fn vacant_slice(&mut self) -> &mut [u8] {
        &mut self.data_vec[self.vacant_idx..]
    }

    pub fn occupied_slice(&mut self) -> &mut [u8] {
        &mut self.data_vec[self.occupied_idx..self.vacant_idx]
    }

    pub fn reuse(&mut self) {
        self.data_vec
            .copy_within(self.occupied_idx..self.vacant_idx, 0);
        self.vacant_idx -= self.occupied_idx;
        self.occupied_idx = 0;
    }

    pub fn reuse_if(&mut self) -> bool {
        if self.vacant_idx == self.capacity {
            self.reuse();
            return true;
        }
        false
    }

    pub fn free(&mut self, consumed: usize) -> Result<()> {
        let new_occupied_idx = self.occupied_idx + consumed;
        if new_occupied_idx > self.vacant_idx {
            anyhow::bail!(
                "simple buf occupied idx {} -> {} > vacant idx {} should not happen",
                self.occupied_idx,
                new_occupied_idx,
                self.vacant_idx,
            )
        };

        self.occupied_idx = new_occupied_idx;
        Ok(())
    }
}

impl fmt::Display for SimpleBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SimpleBuf([0..{}..{}..{}] = {})",
            self.occupied_idx,
            self.vacant_idx,
            self.capacity,
            self.vacant_idx - self.occupied_idx,
        )
    }
}
