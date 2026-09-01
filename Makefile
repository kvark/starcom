PREFIX ?= $(HOME)/.local
BINDIR = $(PREFIX)/bin
DATADIR = $(PREFIX)/share
APPDIR = $(DATADIR)/applications
ICONDIR = $(DATADIR)/icons/hicolor/scalable/apps

.PHONY: build install uninstall

build:
	cargo build --release --locked --bin starcom

install: build
	# GNU install -D creates parent directories; BSD/macOS install does not,
	# and fails with a temp name like INS@xxxxx in the missing directory.
	mkdir -p "$(BINDIR)" "$(ICONDIR)" "$(APPDIR)"
	install -m 755 target/release/starcom "$(BINDIR)/starcom"
	install -m 644 etc/starcom.svg "$(ICONDIR)/starcom.svg"
	sed 's|Exec=starcom|Exec=$(BINDIR)/starcom|' etc/starcom.desktop \
		> "$(APPDIR)/starcom.desktop"
	chmod 644 "$(APPDIR)/starcom.desktop"
	-update-desktop-database "$(APPDIR)" >/dev/null 2>&1
	-gtk-update-icon-cache -f -t "$(DATADIR)/icons/hicolor" >/dev/null 2>&1
	@echo "Installed to $(PREFIX). Make sure $(BINDIR) is in your PATH."

uninstall:
	rm -f "$(BINDIR)/starcom"
	rm -f "$(APPDIR)/starcom.desktop"
	rm -f "$(ICONDIR)/starcom.svg"
