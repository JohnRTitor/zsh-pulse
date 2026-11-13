NAME := zsh-pulse

PREFIX ?= /usr
LIBDIR ?= $(PREFIX)/lib
DATADIR ?= $(PREFIX)/share

CARGO_TARGET_DIR ?= target
DEBUG ?= 0
ifeq ($(DEBUG),0)
	TARGET := release
	PROFILE_ARGS := --release
else
	TARGET := debug
	PROFILE_ARGS :=
endif

MODULE_SRC := $(CARGO_TARGET_DIR)/$(TARGET)/libzshpulse_core.so
MODULE_LIB_SUBDIR ?= $(NAME)/zshpulse
MODULE_PATH_DIR ?= $(LIBDIR)/$(NAME)
MODULE_DST_DIR := $(DESTDIR)$(LIBDIR)/$(MODULE_LIB_SUBDIR)
ADAPTER_DST_DIR := $(DESTDIR)$(DATADIR)/$(NAME)

.PHONY: all build clean install install-files install-init

all: build

build:
	cargo build $(PROFILE_ARGS)

clean:
	cargo clean

install-files:
	install -d -m 0755 $(MODULE_DST_DIR)
	install -Dm0755 $(MODULE_SRC) $(MODULE_DST_DIR)/core.so
	install -d -m 0755 $(ADAPTER_DST_DIR)/zsh
	install -Dm0644 zsh/init.zsh $(ADAPTER_DST_DIR)/zsh/init.zsh
	install -Dm0644 zsh/widgets.zsh $(ADAPTER_DST_DIR)/zsh/widgets.zsh
	install -Dm0644 zsh/completion.zsh $(ADAPTER_DST_DIR)/zsh/completion.zsh

install-init:
	@printf '%s\n' \
	  '# zsh-pulse — entry point for a system installation.' \
	  '# Source this from a .zshrc:' \
	  '#   source $(DATADIR)/$(NAME)/init.zsh' \
	  '' \
	  'export ZSH_PULSE_MODULE_PATH="$(MODULE_PATH_DIR)"' \
	  'source "$(DATADIR)/$(NAME)/zsh/init.zsh"' \
	  > $(ADAPTER_DST_DIR)/init.zsh
	chmod 0644 $(ADAPTER_DST_DIR)/init.zsh

install: install-files install-init
