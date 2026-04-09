<p align="center">
  <br>
  <img src="https://img.shields.io/badge/rust-nightly-orange?style=flat-square&logo=rust" alt="Rust Nightly">
  <img src="https://img.shields.io/badge/target-x86__64--bare--metal-blue?style=flat-square" alt="x86_64">
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT">
  <img src="https://img.shields.io/badge/status-active%20development-brightgreen?style=flat-square" alt="Status">
  <br><br>
</p>

<h1 align="center">Squirrel AIOS</h1>

<p align="center">
  <strong>The operating system where AI isn't a guest — it's the architect.</strong>
  <br>
  A bare-metal OS written from scratch in Rust, designed from first principles for AI sovereignty.
  <br><br>
  <em>UNIX said "everything is a file." Squirrel says "everything is an AI-accessible semantic intent."</em>
</p>

---

## What is this?

Every existing operating system treats AI as an application — a process running inside an architecture designed in the 1970s for humans typing commands. Squirrel AIOS inverts this entirely.

**AI is the primary principal of this operating system.** It doesn't run *on* the OS. It runs *as* the OS. The kernel, the scheduler, the filesystem, the IPC mechanism — every layer is designed for an intelligence that reasons, not one that merely executes.

This isn't Linux with an AI wrapper. This is what an operating system looks like when you throw away fifty years of assumptions and ask: *what if the machine's primary user was a mind?*

![WhatsApp Image 2026-04-08 at 11 34 09](https://github.com/user-attachments/assets/7ad2504d-33df-430a-befb-a593585ac88f)
![WhatsApp Image 2026-04-08 at 11 36 07](https://github.com/user-attachments/assets/a69b3dc8-d173-452d-b353-27f2e926db4f)
![WhatsApp Image 2026-04-08 at 11 52 26](https://github.com/user-attachments/assets/2938594c-584e-4cda-b00c-fc1fa7be7747)


## Architecture

```
┌─────────────────────────────────────────────────┐
│                 Primary Agent                    │  ← AI that talks to you
├─────────────────────────────────────────────────┤
│  Inference Engine   │  Settings  │  Glass Box   │  ← AI backends + live state
├─────────────────────────────────────────────────┤
│     SART (Agent Runtime)  │  Intent Bus (IPC)   │  ← agents are processes
├─────────────────────────────────────────────────┤
│   WASM Modules  │   SVFS (Semantic FS)          │  ← capabilities + storage
├─────────────────────────────────────────────────┤
│  Memory (PMM/VMM/Heap)  │  Network (TCP/TLS)    │  ← hardware abstraction
├─────────────────────────────────────────────────┤
│           Bare-Metal Rust Kernel (x86_64)        │  ← no Linux, no POSIX
└─────────────────────────────────────────────────┘
            Limine Bootloader → UEFI/BIOS
```

## The Six Principles

| Principle | What it means |
|-----------|--------------|
| **Intent Bus** | Agents don't make syscalls. They emit semantic intents — typed, auditable messages like `InferenceRequest` or `StoreObject`. The OS understands *what you want*, not just *what bytes to move*. |
| **Glass Box Execution** | No opaque processes. Every agent's live state is a readable semantic surface. The AI doesn't guess what's running — it *sees* inside everything, in real time. |
| **Capability Fabric** | Apps are dead. Modules are WASM components that expose their full capability surface. The AI composes them like tools, not like applications with UIs to click through. |
| **Semantic VFS** | Files are dead too. SVFS stores *objects* — content-addressed, tagged, with meaning and relationships. The AI doesn't navigate paths; it queries by semantics. |
| **Agent Runtime (SART)** | Processes that *reason*. Agents are first-class OS entities with priorities, heartbeats, and intent subscriptions. The scheduler knows they think. |
| **Bare Metal AI** | The GPU isn't a display adapter. It's cognitive substrate. Hardware accelerators are exposed directly to the inference engine — no driver abstraction tax. |

## Tech Stack

Everything is Rust. No C runtime. No libc. No POSIX. No exceptions.

- **Kernel:** `#![no_std]` Rust on `x86_64-unknown-none`
- **Boot:** Limine protocol (UEFI + BIOS)
- **Memory:** Custom PMM → VMM → heap allocator
- **Agents:** Cooperative async runtime with priority scheduling
- **IPC:** Zero-copy intent bus with typed payloads
- **Modules:** WebAssembly (wasmi interpreter)
- **Storage:** Content-addressed with blake3 hashing
- **Network:** smoltcp (TCP/IP) + rustls (TLS 1.3)
- **Inference:** llama.cpp (local) + cloud APIs (OpenAI, Anthropic, Gemini)
- **Crypto:** AES-256-GCM for API key storage

## Quick Start

```bash
# Clone
git clone https://github.com/tusharkhatriofficial/squirrel.git
cd squirrel

# Build (Rust nightly auto-installed via rust-toolchain.toml)
make -f build/Makefile build

# Run in QEMU (virtio-net, 4GB RAM)
make -f build/Makefile run
```

**Requirements:** Rust nightly, QEMU, xorriso, nasm

### AI Backend Setup (Required)

Squirrel needs a cloud AI API key to work. There are two ways to set it up:

**Option A — Hardcode before building (recommended for quick setup):**

Open `kernel/src/main.rs`, find **line 197** (`settings::init();`), and add this line right after it:

```rust
inference_engine::configure("gemini", "YOUR_API_KEY", "gemini-2.5-flash", "");
```

So it looks like:
```rust
    settings::init();

    inference_engine::configure("gemini", "YOUR_API_KEY", "gemini-2.5-flash", "");

    // 8. APIC — disable legacy PIC, enable Local APIC, start 100 Hz timer
```

> **Do not commit your API key.** This is a local-only change for running in QEMU.

**Option B — Configure at runtime:**

Boot the OS without a key, then type `settings` in the Squirrel shell. It will show a menu to select a backend and enter your API key. This works but typing a long API key in the QEMU console can be tedious.

**Supported backends:**

| Backend | Provider arg | Model examples |
|---------|-------------|----------------|
| Gemini | `"gemini"` | `"gemini-2.5-flash"`, `"gemini-2.0-flash"` |
| OpenAI | `"openai"` | `"gpt-4o"`, `"gpt-4o-mini"` |
| Anthropic | `"anthropic"` | `"claude-sonnet-4-6"`, `"claude-haiku-4-5-20251001"` |

### QEMU Targets

```bash
make -f build/Makefile run            # virtio-net (default, fastest)
make -f build/Makefile run-e1000      # Intel e1000 NIC (real hardware driver)
make -f build/Makefile run-rtl8139    # Realtek RTL8139 NIC
make -f build/Makefile debug          # Launch with GDB attached
```

## What it looks like

```
[OK] GDT
[OK] IDT
[OK] Memory: PMM 512 frames free
[OK] Heap: Box::new works
[OK] APIC + timer (100 Hz)
[OK] Intent Bus
[OK] SART: agents registered
[OK] SVFS formatted
[OK] Network stack ready
[OK] Inference engine: local backend ready

  ╔══════════════════════════════════════╗
  ║   Squirrel AIOS                      ║
  ║   AI Sovereign Operating System      ║
  ╚══════════════════════════════════════╝

  Type anything. Type 'help' for commands.

> what do you see?
[thinking...]
I can see 8 agents running, SVFS has 3 objects stored,
network is connected at 10.0.2.15, inference latency is 240ms...
[local-gguf in 1842ms]

> _
```

## Project Structure

```
squirrel/
├── kernel/              # Bare-metal kernel (GDT, IDT, memory, drivers)
├── intent-bus/          # Semantic IPC — the nervous system
├── sart/                # Agent Runtime — processes that reason
├── wasm-runtime/        # WASM module host — the capability fabric
├── svfs/                # Semantic VFS — storage with meaning
├── glass-box/           # Live state inspection — no opaque processes
├── network/             # TCP/IP + TLS — connection to the world
├── inference-engine/    # Local + cloud AI — the mind
├── settings/            # OS config with encrypted key storage
├── primary-agent/       # The AI that faces the user
├── modules/             # WASM capability modules
└── build/               # Linker script, bootloader, Makefile
```

## Why?

Because the API economy is a cage. Every "AI-powered" tool today is an AI squeezed into a POSIX-shaped box, begging the kernel for file descriptors and socket handles through interfaces designed before neural networks existed.

Squirrel asks: what if the OS was *born* understanding intelligence? What if scheduling knew about reasoning costs? What if the filesystem stored meaning, not just bytes? What if every process was transparent to the mind running on the same machine?

This is that OS.

## Status

Active development. Building in public, one phase at a time.

## License

MIT

---

<p align="center">
  <em>Founded by Tushar Khatri</em>
  <br>
  <em>The first operating system that knows it's thinking.</em>
</p>
