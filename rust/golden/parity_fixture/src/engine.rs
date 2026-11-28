pub struct Engine {
    pub depth: u32,
}

impl Engine {
    pub fn new() -> Self {
        Self { depth: 0 }
    }

    pub fn bump(&mut self) {
        self.depth += 1;
    }
}

pub fn start() -> Engine {
    Engine::new()
}
