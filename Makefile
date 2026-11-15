.DEFAULT_GOAL := all

NAME := zsh-pulse

# Installation directories.
# Override PREFIX to relocate the entire installation.
#
# Example:
#   make install PREFIX=/usr
#   make install PREFIX=/opt/zsh-pulse
#   make install PREFIX=/usr DESTDIR=/tmp/package

PREFIX ?= /usr
DESTDIR ?=

LIBDIR ?= $(PREFIX)/lib
DATADIR ?= $(PREFIX)/share

INSTALL ?= install
ZSH ?= zsh
CARGO ?= cargo

# Cargo build configuration.
CARGO_TARGET_DIR ?= target

DEBUG ?= 0

ifeq ($(DEBUG),0)
	TARGET ?= release
else
	TARGET ?= debug
endif

ifeq ($(TARGET),release)
	PROFILE_ARGS := --release
else ifeq ($(TARGET),debug)
	PROFILE_ARGS :=
else
	PROFILE_ARGS := --profile $(TARGET)
endif

# Installation layout:
#
# Default:
#   $(LIBDIR)/zsh-pulse/zshpulse/core.so
#
# Custom:
#   $(LIBDIR)/zshpulse/core.so
#
# MODULE_LIB_SUBDIR is relative to LIBDIR.
MODULE_LIB_SUBDIR ?= $(NAME)/zshpulse

MODULE_PATH_DIR ?= $(patsubst %/,%,$(dir $(LIBDIR)/$(MODULE_LIB_SUBDIR)))

MODULE_SRC = $(CARGO_TARGET_DIR)/$(TARGET)/libzshpulse_core.so

MODULE_DST_DIR = $(DESTDIR)$(LIBDIR)/$(MODULE_LIB_SUBDIR)
ADAPTER_DST_DIR = $(DESTDIR)$(DATADIR)/$(NAME)


.PHONY: all build clean
.PHONY: install install-files install-init install-zcompile


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

all: build

build:
	$(CARGO) build $(PROFILE_ARGS)

clean:
	$(CARGO) clean


# ---------------------------------------------------------------------------
# Installation
# ---------------------------------------------------------------------------

# Install the native module and Zsh adapter files.
#
# install -D creates parent directories automatically.

install-files:
	$(INSTALL) -Dm0755 "$(MODULE_SRC)" "$(MODULE_DST_DIR)/core.so"

	$(INSTALL) -Dm0644 zsh/init.zsh \
		"$(ADAPTER_DST_DIR)/zsh/init.zsh"

	$(INSTALL) -Dm0644 zsh/widgets.zsh \
		"$(ADAPTER_DST_DIR)/zsh/widgets.zsh"

	$(INSTALL) -Dm0644 zsh/completion.zsh \
		"$(ADAPTER_DST_DIR)/zsh/completion.zsh"


# Generate the system installation entrypoint.
#
# DESTDIR is intentionally excluded from the paths embedded in this file:
# DESTDIR is only a staging directory, not the final installation prefix.

install-init: install-files
	$(INSTALL) -d -m0755 "$(ADAPTER_DST_DIR)"

	@printf '%s\n' \
	  '# zsh-pulse — entry point for a system installation.' \
	  '# Source this from a .zshrc:' \
	  '#   source $(DATADIR)/$(NAME)/init.zsh' \
	  '' \
	  'export ZSH_PULSE_MODULE_PATH="$(MODULE_PATH_DIR)"' \
	  'source "$(DATADIR)/$(NAME)/zsh/init.zsh"' \
	  > "$(ADAPTER_DST_DIR)/init.zsh"

	chmod 0644 "$(ADAPTER_DST_DIR)/init.zsh"


# Precompile the installed Zsh scripts into wordcode (.zwc).
#
# Depends on install-init so the entrypoint exists before compilation.
# Nix can invoke this target directly; install-files/install-init run
# transitively through the dependency chain without rebuilding Cargo.

install-zcompile: install-init
	$(ZSH) -fc 'zcompile "$$1"' zsh "$(ADAPTER_DST_DIR)/init.zsh"
	$(ZSH) -fc 'zcompile "$$1"' zsh "$(ADAPTER_DST_DIR)/zsh/init.zsh"
	$(ZSH) -fc 'zcompile "$$1"' zsh "$(ADAPTER_DST_DIR)/zsh/widgets.zsh"
	$(ZSH) -fc 'zcompile "$$1"' zsh "$(ADAPTER_DST_DIR)/zsh/completion.zsh"


# Full installation.
#
# Dependency chain guarantees correct order:
#   install-files -> install-init -> install-zcompile
# .NOTPARALLEL is kept for the aggregate target as a safety net.

.NOTPARALLEL: install

install: all install-zcompile