# Squirrel AIOS — User Guide

A plain-English guide to understanding, building, and running Squirrel AIOS.

---

## Part 1: How Squirrel AIOS Works (The Simple Version)

### What is it?

Squirrel AIOS is an operating system — like Windows or macOS — but built from scratch for AI. There is no Linux underneath. No Windows. It runs directly on your computer's hardware (or in a virtual machine).

When you turn it on, instead of a desktop with icons, you get a conversation. You type something, and an AI answers. The AI calls out to cloud providers (OpenAI, Anthropic, or Google Gemini) over a real network connection that the OS itself manages — no browser, no app, no middleman.

### What happens when you turn it on?

Here is the boot sequence, step by step:

```
Power on
   |
   v
1. BIOS/UEFI firmware loads from your motherboard
   (this is built into every computer — it finds the boot drive)
   |
   v
2. Limine bootloader starts
   (a small program on the USB/CD that knows how to load our kernel)
   |
   v
3. Squirrel kernel starts
   (our Rust code takes over the entire machine)
   |
   v
4. Hardware setup
   - GDT: tells the CPU how memory segments work
   - IDT: sets up interrupt handlers (so the CPU knows what to do
     when you press a key, when the timer ticks, etc.)
   - Memory manager: maps out all available RAM, sets up page tables
   - Heap allocator: enables dynamic memory (Box, Vec, String all work)
   - APIC timer: a 100 Hz heartbeat that drives the whole system
   |
   v
5. Core systems start
   - Intent Bus: the message backbone — every part of the OS talks
     by sending typed messages ("intents") through this bus.
     A self-test runs to verify it works (sends 42, checks 42 arrives).
   - SVFS: the filesystem — stores data as tagged, content-addressed
     objects in RAM. A self-test stores and retrieves an object to verify.
   - OS Settings: loads saved configuration from SVFS (or creates defaults).
     This is where your AI backend choice and API keys are remembered.
   |
   v
6. Network card detection
   - The OS scans the PCI bus for a supported NIC:
     - Intel e1000/e1000e (I217, I218, I219 — most Intel PCs since 2012)
     - Realtek RTL8139 (budget PCs, older machines)
     - Virtio-net (QEMU virtual machines)
   - If found, the OS runs DHCP to get an IP address, gateway, and
     DNS server. This gives the OS real internet access.
   - If no supported NIC is found, the OS continues without networking.
   - PS/2 Keyboard: so you can type
   |
   v
7. WASM modules load (5 modules, embedded inside the kernel binary)
   - hello-module: prints "Hello from WASM!" to prove WASM works
   - display-module: handles printing text to the screen
   - input-module: handles keyboard input, line editing, backspace
   - storage-module: bridges between agents and the SVFS filesystem
   - settings-module: the settings screen UI

   Each module becomes a scheduled agent, just like native Rust code.
   If any module fails to load, the system prints a warning and continues.
   |
   v
8. Agents register (the "processes" of Squirrel AIOS)
   - Heartbeat Agent: ticks once per second (the OS pulse)
   - Echo Agent: listens for heartbeats (proves agents can talk)
   - Glass Box Agent: watches all agents and can display their live state
   - Network Agent: handles HTTP/HTTPS requests for other agents
   - Inference Router: receives AI requests and routes them to the
     right backend (cloud API in this MVP)
   - Keyboard Bridge Agent: converts raw keystrokes from the
     interrupt handler into Intent Bus messages
   - Primary AI Agent: the main intelligence — reads your input,
     decides what to do, talks to the AI, shows you the response
   |
   v
9. Boot complete — you see:

   Squirrel AIOS v0.1.0
   [OK] GDT
   [OK] IDT
   [OK] Memory
   [OK] Heap: Box=0xDEADBEEF, Vec len=5, String=Squirrel
   [OK] Intent Bus
   [OK] Intent Bus self-test passed (value=42)
   [OK] SVFS initialized (0 objects)
   [OK] SVFS self-test: store+retrieve verified
   [OK] OS Settings loaded
   [OK] APIC + timer (100 Hz)
   [OK] Keyboard
   [OK] Network stack ready
   [OK] WASM: hello-module loaded
   [OK] WASM: display-module loaded
   [OK] WASM: input-module loaded
   [OK] WASM: storage-module loaded
   [OK] WASM: settings-module loaded
   [OK] SART: 14 agents registered
   [OK] Interrupts enabled — SART running

   ╔══════════════════════════════════════╗
   ║   Squirrel AIOS                      ║
   ║   AI Sovereign Operating System      ║
   ╚══════════════════════════════════════╝

   Type anything. Type 'help' for commands.

   > _
```

### How the AI works in this MVP

Squirrel AIOS does **not** bundle a local AI model in this release. The inference engine framework for running local GGUF models is fully written, but no model is included (they're too large to embed in a 3.5 MB ISO).

Instead, the AI works by calling **cloud APIs** over the network:

- **OpenAI** (GPT-4o, etc.)
- **Anthropic** (Claude)
- **Google Gemini**
- **Any OpenAI-compatible API** (self-hosted, ollama, etc.)

The OS makes real HTTPS calls with TLS 1.3 encryption. It builds the HTTP request, sends it through the NIC driver (Intel e1000, Realtek RTL8139, or virtio-net), receives the response, parses the JSON, and displays the answer — all from bare metal, no OS underneath.

**You need an API key** from one of these providers to use the AI features. Without one, the AI prompt will show an error when you try to chat. The built-in commands (`help`, `status`, `settings`, `clear`) work without any API key.

### What happens when you type something?

```
You type "hello" and press Enter
   |
   v
Keyboard interrupt fires (hardware level)
   |
   v
Keyboard Bridge Agent picks up the characters
and sends "input.char" messages on the Intent Bus
   |
   v
Input Module (WASM) collects characters into a line,
handles backspace, shows what you type on screen
   |
   v
You press Enter → Input Module sends "input.line"
with the text "hello"
   |
   v
Primary AI Agent receives "input.line"
   |
   v
Planner checks: is this a built-in command?
   - "help"     → show help text (instant, no AI needed)
   - "settings" → open settings screen
   - "clear"    → clear the screen
   - "status"   → show system status (agents, backend, tick count)
   - anything else → send to AI
   |
   v
"hello" is not a command → sent to the Inference Router
   |
   v
Inference Router reads current settings:
   - Which backend? (openai / anthropic / gemini / custom)
   - What API key? (decrypted from SVFS on demand)
   - What model? (gpt-4o, claude-sonnet, gemini-pro, etc.)
   |
   v
Builds an HTTPS request for that provider:
   - OpenAI:    POST https://api.openai.com/v1/chat/completions
   - Anthropic: POST https://api.anthropic.com/v1/messages
   - Gemini:    POST https://generativelanguage.googleapis.com/...
   |
   v
Network Agent sends the request through:
   NIC driver (e1000/RTL8139/virtio) → TCP → TLS 1.3 → HTTPS
   |
   v
Cloud API returns a response (JSON)
   |
   v
Inference Router parses the response, measures latency
   |
   v
Primary Agent displays the answer:

   > hello
   [thinking...]
   Hello! I'm running on Squirrel AIOS...
   [anthropic (claude-sonnet) in 380ms]

   > _
```

### What can you do at the prompt?

| Command | What it does |
|---------|-------------|
| `help` | Shows available commands |
| `status` | Shows system info — how many agents, which AI backend, tick count |
| `settings` | Opens settings screen to switch AI backend and enter API keys |
| `clear` | Clears the screen |
| Anything else | Sends to the cloud AI and shows its response |

### Setting up an AI backend

On first boot, the default backend is "local" — but since no local model is bundled, you need to switch to a cloud provider:

1. Type `settings`
2. Pick a provider (e.g., Anthropic)
3. Enter your API key (typed characters are hidden)
4. Done — type anything and the AI responds

Your API key is **encrypted with AES-256-GCM** and stored in SVFS. It's never logged or displayed. The encryption key is derived from your CPU's unique identifier.

You can switch providers anytime. The switch is instant — the next message you type goes to the new provider. No reboot needed.

---

## Part 2: Building the ISO

The ISO is a bootable disk image. You can boot it in QEMU (a virtual machine) or write it to a USB drive for real hardware.

### Prerequisites

Install these on your computer:

| Tool | What it is | macOS | Linux |
|------|-----------|-------|-------|
| **Rust nightly** | The programming language | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | Same |
| **WASM target** | Lets Rust compile to WebAssembly | `rustup target add wasm32-unknown-unknown` | Same |
| **QEMU** | Virtual machine emulator | `brew install qemu` | `sudo apt install qemu-system-x86` |
| **xorriso** | Creates bootable ISO files | `brew install xorriso` | `sudo apt install xorriso` |

Rust nightly is selected automatically — the repo has a `rust-toolchain.toml` that handles this.

### Build steps

```bash
# 1. Navigate to the squirrel directory
cd squirrel

# 2. Build everything (WASM modules first, then kernel)
make -f build/Makefile build
```

This does two things in order:
- Compiles all 5 WASM modules to `.wasm` files
- Compiles the kernel, which embeds those WASM binaries inside itself via `include_bytes!`

```bash
# 3. Build the bootable ISO
make -f build/Makefile iso
```

This packages the kernel with the Limine bootloader into a single bootable ISO file.

Output: `target/squirrel.iso` (~3.5 MB)

```bash
# 4. Create a virtual disk for storage (optional, for QEMU)
make -f build/Makefile disk
```

Creates a 2 GB raw disk image at `target/squirrel-disk.img`. This is used by QEMU as a virtual hard drive. Without it, SVFS uses RAM-only storage and settings are lost on shutdown.

---

## Part 3: Running in QEMU (Virtual Machine)

QEMU lets you run Squirrel AIOS inside your current OS — like an emulator. Nothing on your real computer is affected.

### Run it

```bash
# Default: virtio-net (fastest in QEMU)
make -f build/Makefile run

# Test with Intel e1000 NIC (same driver as real Intel hardware)
make -f build/Makefile run-e1000

# Test with Realtek RTL8139 NIC
make -f build/Makefile run-rtl8139
```

The default `run` launches QEMU with:
- The ISO as a virtual CD-ROM
- A 2 GB virtual disk for SVFS storage
- A virtual network card with internet access
- 4 GB of RAM
- Serial output piped to your terminal

Use `run-e1000` or `run-rtl8139` to test the bare metal drivers in QEMU before writing to USB.

### What you will see

The boot log appears in your terminal (not the QEMU graphical window). After all the `[OK]` lines, the Squirrel prompt appears. Type directly in the terminal.

### First thing to do after boot

The default AI backend is "local" but no local model is bundled, so:

1. Type `settings`
2. Choose a cloud provider (OpenAI, Anthropic, or Gemini)
3. Enter your API key
4. Press Escape or follow the prompts to return
5. Now type anything — the AI will respond via the cloud

### Exiting QEMU

Press `Ctrl+A` then `X` (press Ctrl+A, release, then press X separately).

### Troubleshooting

| Problem | Fix |
|---------|-----|
| `qemu-system-x86_64: command not found` | Install QEMU: `brew install qemu` (macOS) or `sudo apt install qemu-system-x86` (Linux) |
| `KVM not available` | Normal on macOS. QEMU falls back to software emulation automatically. Slower but works. |
| No output visible | Look at the terminal, not the QEMU graphical window. Serial output goes to your terminal. |
| AI says no backend / error | You need to run `settings` and enter an API key first. |
| `xorriso: command not found` | `brew install xorriso` (macOS) or `sudo apt install xorriso` (Linux) |

---

## Part 4: Running on Real Hardware (Bare Metal)

This boots Squirrel AIOS directly on a physical x86_64 computer. No other OS involved.

### What you need

- A USB drive (at least 16 MB — the ISO is only ~3.5 MB)
- An x86_64 computer (any 64-bit Intel or AMD PC/laptop)
- The built ISO file (`target/squirrel.iso`)

### Step 1: Write the ISO to a USB drive

**On macOS:**
```bash
# Find your USB drive
diskutil list
# Look for your USB drive (e.g., /dev/disk2)
# DOUBLE CHECK — wrong disk = you wipe that drive's data

# Unmount it
diskutil unmountDisk /dev/diskN    # replace N with your disk number

# Write the ISO (use rdiskN for speed)
sudo dd if=target/squirrel.iso of=/dev/rdiskN bs=4m status=progress

# Eject
diskutil eject /dev/diskN
```

**On Linux:**
```bash
# Find your USB drive
lsblk
# Look for your USB drive (e.g., /dev/sdb)

# Write the ISO (replace /dev/sdX with your drive)
sudo dd if=target/squirrel.iso of=/dev/sdX bs=4M status=progress
sync
```

### Step 2: Boot from the USB

1. Plug the USB into the target computer
2. Turn on (or restart) the computer
3. Enter the boot menu — press `F12`, `F2`, `F10`, or `Del` during startup (varies by manufacturer; look for "Press F12 for Boot Menu" on screen)
4. Select the USB drive
5. Limine loads, then the Squirrel kernel starts
6. You see the boot sequence in text mode

### Supported network cards

Squirrel AIOS auto-detects your NIC on the PCI bus. These are supported:

| NIC | Vendor ID | Common in |
|-----|-----------|-----------|
| **Intel e1000/e1000e** (I217, I218, I219) | 0x8086 | Most Intel desktops/laptops since 2012 |
| **Realtek RTL8139** | 0x10EC | Budget PCs, older machines |
| **Virtio-net** | 0x1AF4 | QEMU virtual machines |

**To check what NIC your PC has:**
- Linux: `lspci | grep -i ethernet`
- Windows: Device Manager → Network Adapters
- macOS: System Information → Network

If your NIC is in the list, the AI works on bare metal. If not, the OS boots but skips networking.

**WiFi is not supported** — you need a wired Ethernet connection.

### Important bare metal notes

- **Plug in an Ethernet cable.** Squirrel needs a wired connection for DHCP and cloud AI. WiFi drivers are extremely complex and not supported.
- **Keyboard works** through PS/2 or USB legacy mode (most BIOS/UEFI firmware emulates PS/2 for USB keyboards automatically).
- **RAM-only storage.** There's no real disk driver yet, so settings and data are lost on reboot. You'll need to re-enter your API key each boot.
- **Your hard drive is untouched.** Squirrel does not read or write your computer's hard drive. Remove the USB and restart to go back to your normal OS.

---

## Part 5: What's inside the ISO

```
squirrel.iso (~3.5 MB)
├── boot/
│   ├── squirrel-kernel          ← the entire OS in one binary (~1.2 MB)
│   └── limine/
│       ├── limine.conf          ← tells the bootloader where to find our kernel
│       ├── limine-bios.sys      ← BIOS boot support
│       ├── limine-bios-cd.bin   ← BIOS CD boot sector
│       └── limine-uefi-cd.bin   ← UEFI CD boot support
└── EFI/
    └── BOOT/
        └── BOOTX64.EFI          ← UEFI boot entry point
```

The kernel binary contains everything:
- The Rust kernel (hardware init, memory manager, interrupt handlers)
- All 5 WASM modules (embedded as byte arrays at compile time)
- The WASM interpreter (wasmi)
- The TCP/IP stack (smoltcp)
- The TLS 1.3 library (for HTTPS)
- The inference engine (cloud API client)
- The SVFS filesystem
- The Intent Bus and SART agent runtime
- The AI system prompt

One file. One binary. The entire operating system.

---

## Part 6: What works and what doesn't (MVP honest list)

### Works

- Boots from USB or CD on real x86_64 hardware
- Full boot sequence with hardware initialization
- Keyboard input and text display
- Intent Bus message passing between all components
- SART agent scheduling with priorities
- 5 WASM capability modules loaded and running
- SVFS content-addressed storage (in RAM)
- Settings system with encrypted API key storage (AES-256-GCM)
- Live backend switching (no reboot to change AI provider)
- Glass Box live state inspection
- **Bare metal networking** with Intel e1000/e1000e and Realtek RTL8139 NICs
- Networking in QEMU with virtio-net, e1000, or rtl8139 emulation
- Full TCP/IP (DHCP, DNS, TCP sockets)
- TLS 1.3 encrypted HTTPS connections
- Cloud AI via OpenAI, Anthropic, Gemini, or any OpenAI-compatible API
- Built-in commands (help, status, settings, clear)

### Doesn't work (yet)

| Limitation | Why | What it means for you |
|-----------|-----|----------------------|
| No local AI model | Models are too large to embed; loading from disk not wired up yet | You need an API key from a cloud provider |
| WiFi not supported | WiFi drivers are very complex (WPA2, firmware, etc.) | Use a wired Ethernet connection |
| Some NICs not supported | Only Intel e1000 family and Realtek RTL8139 | Check `lspci` — most Intel PCs work |
| RAM-only storage | No real disk driver (only virtio-blk for QEMU) | Settings lost on bare metal reboot |
| Single CPU | No SMP (symmetric multiprocessing) | Only uses 1 core |
| No preemption | Agents cooperate; inference blocks everything | UI freezes during AI generation |
| Certificate verification off | TLS encrypts but doesn't verify server identity | Security limitation for MVP |

---

## Quick Reference

```bash
# Build everything (WASM modules + kernel)
make -f build/Makefile build

# Build the bootable ISO
make -f build/Makefile iso

# Run in QEMU with virtio-net
make -f build/Makefile run

# Run in QEMU with Intel e1000 (tests bare metal driver)
make -f build/Makefile run-e1000

# Run in QEMU with Realtek RTL8139 (tests bare metal driver)
make -f build/Makefile run-rtl8139

# Clean all build artifacts
make -f build/Makefile clean
```

---

*Squirrel AIOS — Founded by Tushar Khatri*
