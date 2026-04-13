VERSION ?= dev

.PHONY: build install clean test release-check snapshot release

build:
	go build -ldflags "-s -w -X github.com/ashon/ax/cmd.version=$(VERSION)" -o ax .

install: build
	cp ax $(shell go env GOPATH)/bin/ax
	codesign -s - $(shell go env GOPATH)/bin/ax

clean:
	rm -f ax

test:
	go test ./...

release-check:
	@test -z "$$(git status --porcelain --untracked-files=all)" || (echo "Working tree is not clean. Commit, stash, or remove changes before releasing." >&2; git status --short >&2; exit 1)
	@$(MAKE) test

snapshot:
	goreleaser release --snapshot --clean

# Usage: make release {patch|minor|major|dev}
LATEST_STABLE := $(shell git tag -l 'v*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$$' | head -1)
LATEST_STABLE := $(or $(LATEST_STABLE),v0.0.0)
CURRENT := $(subst v,,$(LATEST_STABLE))
MAJOR := $(word 1,$(subst ., ,$(CURRENT)))
MINOR := $(word 2,$(subst ., ,$(CURRENT)))
PATCH := $(word 3,$(subst ., ,$(CURRENT)))

release: release-check
ifeq ($(filter $(word 2,$(MAKECMDGOALS)),patch minor major dev),)
	$(error Usage: make release {patch|minor|major|dev})
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
ifeq ($(word 2,$(MAKECMDGOALS)),dev)
	$(eval NEXT_PATCH := v$(MAJOR).$(MINOR).$(shell echo $$(($(PATCH)+1))))
	$(eval DEV_NUM := $(shell echo $$(( $(words $(shell git tag -l '$(NEXT_PATCH)-dev*')) + 1 ))))
	$(eval NEXT := $(NEXT_PATCH)-dev$(DEV_NUM))
endif
	@echo "$(LATEST_STABLE) -> $(NEXT)"
	@git tag $(NEXT)
	@git push origin $(NEXT)
	@echo "Released $(NEXT)"

patch minor major dev:
	@:
