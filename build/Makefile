ARCH        := x86_64
KERNEL      := target/x86_64-unknown-none/release/squirrel-kernel
ISO_DIR     := target/iso
ISO         := target/squirrel.iso
LIMINE_DIR  := build/limine
QEMU        := qemu-system-x86_64
QEMU_FLAGS  := -m 4G -serial stdio -vga std

# For QEMU development disk (SVFS storage)
DISK_IMG    := target/squirrel-disk.img
DISK_SIZE   := 2G

.PHONY: build run debug iso disk clean

build:
	cargo build --release -p squirrel-kernel

run: build disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom $(ISO) \
		-drive file=$(DISK_IMG),format=raw,if=virtio \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
		-enable-kvm 2>/dev/null || \
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom $(ISO) \
		-drive file=$(DISK_IMG),format=raw,if=virtio \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0

debug: build disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom $(ISO) \
		-drive file=$(DISK_IMG),format=raw,if=virtio \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
		-s -S &
	gdb $(KERNEL) \
		-ex "set arch i386:x86-64" \
		-ex "target remote :1234" \
		-ex "break _start"

iso: build
	mkdir -p $(ISO_DIR)/boot/limine
	cp $(KERNEL) $(ISO_DIR)/boot/squirrel-kernel
	cp build/limine.conf $(ISO_DIR)/boot/limine/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(ISO_DIR)/boot/limine/
	cp $(LIMINE_DIR)/limine-bios-cd.bin $(ISO_DIR)/boot/limine/
	cp $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_DIR)/boot/limine/
	mkdir -p $(ISO_DIR)/EFI/BOOT
	cp $(LIMINE_DIR)/BOOTX64.EFI $(ISO_DIR)/EFI/BOOT/
	xorriso -as mkisofs \
		-b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		$(ISO_DIR) -o $(ISO)
	$(LIMINE_DIR)/limine bios-install $(ISO)

disk:
	@if [ ! -f $(DISK_IMG) ]; then \
		echo "Creating disk image $(DISK_SIZE)..."; \
		dd if=/dev/zero of=$(DISK_IMG) bs=1M count=2048 status=progress; \
	fi

clean:
	cargo clean
	rm -rf $(ISO_DIR) $(ISO)
