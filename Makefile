PREFIX ?= /usr/local

build:
	cargo build --release

install: build
	ln -sf $(CURDIR)/target/release/koen $(PREFIX)/bin/koen
	@test -f $(HOME)/.koenrc || cp koenrc.example $(HOME)/.koenrc
	@echo "config: $(HOME)/.koenrc"

uninstall:
	rm -f $(PREFIX)/bin/koen

test: build
	cargo test --release
	./test_harness.sh
