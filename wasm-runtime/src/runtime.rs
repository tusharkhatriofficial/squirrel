//! WasmModule — loads, validates, and instantiates WASM binaries.
//!
//! A WasmModule is the bridge between raw `.wasm` bytes and a running
//! SART agent. The lifecycle is:
//!
//!   1. load()     — Parse and validate the WASM binary
//!   2. instantiate() — Create a VM instance with host functions linked
//!   3. The result is a ModuleAgent that SART can schedule
//!
//! In the MVP, WASM modules are embedded at compile time with
//! `include_bytes!`. In Phase 07, they'll be loaded from SVFS.

use wasmi::{Engine, Linker, Module, Store};
use alloc::{format, string::String, vec::Vec};
use crate::host_abi::{register_host_functions, HostState};

/// A parsed WASM module ready to be instantiated.
///
/// Think of this like an executable file on disk — it's been validated
/// but isn't running yet. Call `instantiate()` to create a live VM.
pub struct WasmModule {
    /// Human-readable name for logging and bus routing
    pub name: String,
    /// The wasmi execution engine (shared config for all instances)
    engine: Engine,
    /// The parsed and validated WASM module
    module: Module,
    /// Intent types this module subscribes to (receives)
    pub subscriptions: Vec<String>,
}

impl WasmModule {
    /// Parse and validate a WASM binary.
    ///
    /// `name` — unique identifier for this module (used in logs, bus routing)
    /// `wasm_bytes` — the raw .wasm binary (from include_bytes! or SVFS)
    /// `subscriptions` — intent types this module wants to receive
    ///
    /// Returns Err if the bytes aren't valid WebAssembly.
    pub fn load(
        name: &str,
        wasm_bytes: &[u8],
        subscriptions: &[&str],
    ) -> Result<Self, WasmError> {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| WasmError::InvalidModule(format!("{:?}", e)))?;

        Ok(Self {
            name: String::from(name),
            engine,
            module,
            subscriptions: subscriptions.iter().map(|s| String::from(*s)).collect(),
        })
    }

    /// Create a live VM instance from this module.
    ///
    /// This links the 5 host functions, creates a fresh wasmi Store
    /// (which holds the module's memory and HostState), and runs the
    /// WASM start function if present.
    ///
    /// The result is a ModuleAgent — a SART-compatible agent wrapping
    /// the WASM instance.
    pub fn instantiate(self) -> Result<crate::module_agent::ModuleAgent, WasmError> {
        let mut store = Store::new(&self.engine, HostState::new(&self.name));

        // Link the 5 host functions into the "squirrel" import namespace
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        register_host_functions(&mut linker);

        // Instantiate: resolves imports, allocates WASM memory, runs start()
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| WasmError::InstantiationFailed(format!("{:?}", e)))?
            .start(&mut store)
            .map_err(|e| WasmError::StartFailed(format!("{:?}", e)))?;

        Ok(crate::module_agent::ModuleAgent::new(
            self.name,
            self.subscriptions,
            store,
            instance,
        ))
    }
}

/// Errors that can occur when loading or running a WASM module.
#[derive(Debug)]
pub enum WasmError {
    /// The bytes aren't valid WebAssembly
    InvalidModule(String),
    /// Host function linking failed (missing import, type mismatch)
    InstantiationFailed(String),
    /// The WASM start function trapped (panicked)
    StartFailed(String),
}
