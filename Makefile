PREFIX ?= $(HOME)/.local
BINDIR = $(PREFIX)/bin
DATADIR = $(PREFIX)/share
APPDIR = $(DATADIR)/applications
ICONDIR = $(DATADIR)/icons/hicolor/scalable/apps
UNAME_S := $(shell uname -s)
# Launchpad looks in ~/Applications or /Applications, not PREFIX.
ifeq ($(filter $(HOME) $(HOME)/%,$(PREFIX)),)
MACAPPDIR ?= /Applications
else
MACAPPDIR ?= $(HOME)/Applications
endif

.PHONY: build install uninstall

build:
	cargo build --release --locked --bin starcom

install: build
	# GNU install -D creates parent directories; BSD/macOS install does not,
	# and fails with a temp name like INS@xxxxx in the missing directory.
	mkdir -p "$(BINDIR)"
	install -m 755 target/release/starcom "$(BINDIR)/starcom"
ifeq ($(UNAME_S),Darwin)
	sh scripts/macos-app.sh target/release/starcom "$(MACAPPDIR)/Starcom.app"
	@echo "Installed $(BINDIR)/starcom and $(MACAPPDIR)/Starcom.app"
else
	mkdir -p "$(ICONDIR)" "$(APPDIR)"
	install -m 644 etc/starcom.svg "$(ICONDIR)/starcom.svg"
	sed 's|Exec=starcom|Exec=$(BINDIR)/starcom|' etc/starcom.desktop \
		> "$(APPDIR)/starcom.desktop"
	chmod 644 "$(APPDIR)/starcom.desktop"
	-update-desktop-database "$(APPDIR)" >/dev/null 2>&1
	-gtk-update-icon-cache -f -t "$(DATADIR)/icons/hicolor" >/dev/null 2>&1
	@echo "Installed to $(PREFIX). Make sure $(BINDIR) is in your PATH."
endif

uninstall:
	rm -f "$(BINDIR)/starcom"
ifeq ($(UNAME_S),Darwin)
	rm -rf "$(MACAPPDIR)/Starcom.app"
else
	rm -f "$(APPDIR)/starcom.desktop"
	rm -f "$(ICONDIR)/starcom.svg"
endif
