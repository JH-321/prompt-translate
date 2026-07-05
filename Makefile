PREFIX ?= /usr/local

build:
	cargo build --release

install: build
	ln -sf $(CURDIR)/target/release/koen $(PREFIX)/bin/koen
	mkdir -p $(HOME)/.claude/skills
	ln -sfn $(CURDIR)/skills/koen $(HOME)/.claude/skills/koen

uninstall:
	rm -f $(PREFIX)/bin/koen $(HOME)/.claude/skills/koen

test: build
	cargo test --release
	./test_harness.sh
