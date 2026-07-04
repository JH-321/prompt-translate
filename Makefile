PREFIX ?= /usr/local

install:
	ln -sf $(CURDIR)/bin/koen $(PREFIX)/bin/koen
	mkdir -p $(HOME)/.claude/skills
	ln -sfn $(CURDIR)/skills/koen $(HOME)/.claude/skills/koen

uninstall:
	rm -f $(PREFIX)/bin/koen $(HOME)/.claude/skills/koen

test:
	python3 test_koen.py
