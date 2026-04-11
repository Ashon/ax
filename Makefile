VERSION ?= dev

.PHONY: build clean test snapshot

build:
	go build -ldflags "-s -w -X github.com/ashon/ax/cmd.version=$(VERSION)" -o ax .

clean:
	rm -f ax

test:
	go test ./...

snapshot:
	goreleaser release --snapshot --clean
