VERSION ?= dev

.PHONY: build clean test snapshot release

build:
	go build -ldflags "-s -w -X github.com/ashon/ax/cmd.version=$(VERSION)" -o ax .

clean:
	rm -f ax

test:
	go test ./...

snapshot:
	goreleaser release --snapshot --clean

# Usage: make release {patch|minor|major}
LATEST_TAG := $(shell git describe --tags --abbrev=0 2>/dev/null || echo "v0.0.0")
CURRENT := $(subst v,,$(LATEST_TAG))
MAJOR := $(word 1,$(subst ., ,$(CURRENT)))
MINOR := $(word 2,$(subst ., ,$(CURRENT)))
PATCH := $(word 3,$(subst ., ,$(CURRENT)))

release:
ifeq ($(filter $(word 2,$(MAKECMDGOALS)),patch minor major),)
	$(error Usage: make release {patch|minor|major})
endif
ifeq ($(word 2,$(MAKECMDGOALS)),patch)
	$(eval NEXT := v$(MAJOR).$(MINOR).$(shell echo $$(($(PATCH)+1))))
endif
ifeq ($(word 2,$(MAKECMDGOALS)),minor)
	$(eval NEXT := v$(MAJOR).$(shell echo $$(($(MINOR)+1))).0)
endif
ifeq ($(word 2,$(MAKECMDGOALS)),major)
	$(eval NEXT := v$(shell echo $$(($(MAJOR)+1))).0.0)
endif
	@echo "$(LATEST_TAG) -> $(NEXT)"
	@git tag $(NEXT)
	@git push origin $(NEXT)
	@echo "Released $(NEXT)"

patch minor major:
	@:
