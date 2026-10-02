CARGO ?= cargo
JOBS  ?= 4

.PHONY: build test unit spec spec-report programs lint bench fuzz clean

build:
	$(CARGO) build --release -j$(JOBS)

# Everything: unit tests, the spec suite (both strategies) and the guest programs.
test:
	$(CARGO) test --release -j$(JOBS)

unit:
	$(CARGO) test --release -j$(JOBS) --lib

spec:
	$(CARGO) test --release -j$(JOBS) --test spec

# Regenerate docs/spec-results.md.
spec-report:
	WISP_SPEC_REPORT=docs/spec-results.md $(CARGO) test --release -j$(JOBS) --test spec

programs:
	$(CARGO) test --release -j$(JOBS) --test programs

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --release --all-targets -j$(JOBS) -- -D warnings

bench: build
	python3 bench/run.py

fuzz:
	$(CARGO) run --release -j$(JOBS) --manifest-path tools/fuzz-diff/Cargo.toml -- --cases 2000

clean:
	$(CARGO) clean
	rm -rf tests/programs/target tools/fuzz-diff/target
