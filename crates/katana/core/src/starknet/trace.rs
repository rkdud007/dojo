use blockifier::state::cached_state::CachedState;
use blockifier::state::state_api::StateReader;
use starknet_api::hash::StarkFelt;

pub struct Trace<S: StateReader> {
    pub read: Vec<StarkFelt>,
    pub write: Vec<StarkFelt>,
    state: CachedState<S>,
}

impl<S: StateReader> Trace<S> {
    pub fn new(state: CachedState<S>) -> Self {
        Self { read: Vec::new(), write: Vec::new(), state }
    }

    pub fn read(&mut self, key: &StarkFelt) -> Option<StarkFelt> {
        let value = self.read(key);
        self.read.push(key.clone());
        value
    }

    pub fn write(&mut self, key: StarkFelt, value: StarkFelt) {
        self.write(key.clone(), value.clone());
        self.write.push(key);
    }

    pub fn commit(self) -> CachedState<S> {
        self.state
    }
}
