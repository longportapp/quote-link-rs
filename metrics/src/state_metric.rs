use std::time::Instant;

pub struct StateMetric {
    name: String,
    last_state: String,
    last_at: Instant,
    init_state: String,
    refresh_at: Instant,
}

impl StateMetric {
    pub fn new(name: &str, init_state: &str) -> Self {
        let now = Instant::now();
        Self {
            name: name.to_string(),
            last_state: init_state.to_string(),
            last_at: now,
            init_state: init_state.to_string(),
            refresh_at: now,
        }
    }
}

impl StateMetric {
    pub fn state_to(&mut self, curr_state: &str) {
        self.last_state = curr_state.to_string();
        self.last_at = Instant::now();
    }

    pub fn metric_to(&mut self, curr_state: &str) {
        let sample = format!("{}_2_{}", self.last_state, curr_state);
        crate::process::cost_milli_sec(&self.name, &sample, self.last_at.elapsed());
        self.last_state = curr_state.to_string();
        self.last_at = Instant::now();
    }

    pub fn state_reset(&mut self) {
        let sample = format!("{}_2_{}", self.init_state, self.init_state);
        crate::process::cost_milli_sec(&self.name, &sample, self.refresh_at.elapsed());
        self.last_state = self.init_state.clone();
        self.refresh_at = Instant::now();
    }
}
