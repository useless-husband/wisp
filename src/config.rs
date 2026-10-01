//! Engine configuration.

/// How function bodies are executed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Translate to internal bytecode and interpret.
    Interpreter,
    /// Compile to native AArch64 code with the baseline compiler; functions it cannot
    /// compile run in the interpreter. Falls back to the interpreter on other hosts.
    Compiler,
}

impl Default for Strategy {
    fn default() -> Self {
        if crate::jit::available() { Strategy::Compiler } else { Strategy::Interpreter }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub strategy: Strategy,
    /// Meter execution with fuel (see [`Store::set_fuel`](crate::Store::set_fuel)).
    pub fuel: bool,
    /// Maximum wasm call depth.
    pub max_call_depth: u32,
    /// Size of the interpreter's value stack, in 64-bit slots.
    pub interp_stack_slots: usize,
    /// Native stack, in bytes, compiled code may use below the point where it is entered.
    pub native_stack_budget: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            strategy: Strategy::default(),
            fuel: false,
            max_call_depth: 100_000,
            interp_stack_slots: 1 << 21,
            native_stack_budget: 4 << 20,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn strategy(mut self, s: Strategy) -> Self {
        self.strategy = s;
        self
    }

    pub fn fuel(mut self, on: bool) -> Self {
        self.fuel = on;
        self
    }

    pub fn max_call_depth(mut self, n: u32) -> Self {
        self.max_call_depth = n;
        self
    }
}

/// A compilation environment shared by modules and stores.
#[derive(Clone, Debug, Default)]
pub struct Engine {
    pub(crate) config: Config,
}

impl Engine {
    pub fn new(config: Config) -> Engine {
        Engine { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}
