PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
SRC    = $(shell find src -name '*.rs')
KATEX_VERSION = 0.16.25
KATEX_URL = https://github.com/KaTeX/KaTeX/releases/download/v$(KATEX_VERSION)/katex.tar.gz

.PHONY: all
all: hashcards-web

vendor/katex:
	@echo "Downloading KaTeX $(KATEX_VERSION)..."
	@mkdir -p vendor
	@curl -L -o vendor/katex.tar.gz $(KATEX_URL)
	@echo "Extracting KaTeX..."
	@tar -xzf vendor/katex.tar.gz -C vendor
	@rm vendor/katex.tar.gz
	@echo "Rewriting font paths in CSS..."
	@sed -i.bak 's|fonts/|/katex/fonts/|g' vendor/katex/katex.min.css
	@rm vendor/katex/katex.min.css.bak
	@echo "KaTeX extracted to vendor/katex"
	@rm vendor/katex/katex.css
	@rm vendor/katex/katex.js
	@rm vendor/katex/katex.mjs
	@rm vendor/katex/katex-swap.css
	@rm vendor/katex/katex-swap.min.css
	@rm vendor/katex/contrib/*.mjs
	@rm vendor/katex/contrib/auto-render.js
	@rm vendor/katex/contrib/auto-render.min.js
	@rm vendor/katex/contrib/copy-tex.js
	@rm vendor/katex/contrib/copy-tex.min.js
	@rm vendor/katex/contrib/mathtex-script-type.js
	@rm vendor/katex/contrib/mathtex-script-type.min.js
	@rm vendor/katex/contrib/mhchem.js
	@rm vendor/katex/contrib/render-a11y-string.js
	@rm vendor/katex/contrib/render-a11y-string.min.js
	@rm vendor/katex/fonts/*.ttf
	@rm vendor/katex/fonts/*.woff

hashcards-web: vendor/katex $(SRC) Cargo.toml Cargo.lock
	cargo build --release
	cp "target/release/hashcards-web" hashcards-web

.PHONY: install
install: hashcards-web
	install -d $(BINDIR)
	install -m 755 hashcards-web $(BINDIR)/hashcards-web

.PHONY: uninstall
uninstall:
	rm -f $(BINDIR)/hashcards-web

# Serve the bundled example collection. Collections resolve under
# {data_dir}/repo, so the cards are staged into that layout first. The
# review database lands in target/example-data/db and is thrown away with it.
.PHONY: example
example:
	rm -rf target/example-data
	mkdir -p target/example-data/repo
	cp -r example target/example-data/repo/cards
	RUST_LOG=debug cargo run -- --config example/hashcards.toml

.PHONY: coverage
coverage:
	cargo llvm-cov --html --open --ignore-filename-regex '(main|error|cli).rs'

.PHONY: clean
clean:
	rm -f hashcards-web
	rm -rf target/example-data
	rm -rf vendor
	cargo clean
