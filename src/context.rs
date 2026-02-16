#[derive(Clone, Debug)]
pub struct Context {
    pub dry_run: bool,
    pub verbose: u8,
}

impl Context {
    pub fn new(dry_run: bool, verbose: u8) -> Self {
        Self { dry_run, verbose }
    }
}
